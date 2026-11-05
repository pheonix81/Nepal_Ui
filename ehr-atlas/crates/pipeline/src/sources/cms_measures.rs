//! CMS Provider Data Catalog — Care Compare measure datasets, loaded into one long table.
//!
//! Each dataset in [`CMS_MEASURE_DATASETS`] is downloaded through the catalog metastore
//! and mapped to (ccn, dataset, measure_id, measure_name, value, …) by a row of
//! `config/measures.csv` (staged as `ref_measures`). Two mapping modes:
//!
//! * `long` – one measure per input row; `measure_id_columns` names the id column(s)
//!   (several are joined with ` / `, e.g. spending-by-claim's Period + Claim Type).
//! * `wide` – one input row per hospital with many score columns; one config row per
//!   score column, with `measure_id_literal` naming the measure.
//!
//! `value_columns` may list several columns separated by `|`; the first non-empty one
//! wins (HCAHPS keeps star rating, answer percent and linear mean in separate columns).
//! "Not Available", "Not Applicable", "N/A", "Too Few to Report" and non-numeric text
//! leave `value` NULL but are kept in `value_raw`.

use crate::config::Paths;
use crate::db::{count, lit};
use crate::download::{
    all_from_manifest, check_csv_header, cms_pdc_distribution, download, DownloadRequest,
};
use anyhow::{ensure, Context, Result};
use duckdb::Connection;
use ehr_atlas_core::manifest::RawFile;
use ehr_atlas_core::sources::{SourceId, CMS_MEASURE_DATASETS};
use std::collections::HashMap;
use tracing::info;

pub const SOURCE: &str = "cms_measures";

pub async fn fetch(paths: &Paths, client: &reqwest::Client, offline: bool) -> Result<Vec<RawFile>> {
    if offline {
        return all_from_manifest(paths, SOURCE);
    }
    let mut out = Vec::new();
    for (dataset_id, short) in CMS_MEASURE_DATASETS {
        let (url, modified, title) = cms_pdc_distribution(client, dataset_id).await?;
        out.push(
            download(
                paths,
                client,
                DownloadRequest {
                    source: SourceId::CmsMeasures.as_str(),
                    url: &url,
                    file_name: &format!("{short}.csv"),
                    upstream_modified: modified,
                    notes: Some(format!("CMS PDC dataset {dataset_id}: {title}")),
                },
            )
            .await?,
        );
    }
    Ok(out)
}

/// `data/raw/cms_measures/2026-09-26_hcahps.csv` → `hcahps`.
fn dataset_of(file: &RawFile) -> Option<String> {
    let stem = file.path.file_stem()?.to_str()?;
    let (_, short) = stem.split_once('_')?;
    Some(short.to_string())
}

#[derive(Debug, Clone)]
struct Mapping {
    dataset: String,
    mode: String,
    ccn_column: String,
    measure_id_columns: Vec<String>,
    measure_id_literal: Option<String>,
    measure_name_column: Option<String>,
    measure_name_literal: Option<String>,
    value_columns: Vec<String>,
    denominator_column: Option<String>,
    compared_column: Option<String>,
    footnote_column: Option<String>,
    start_column: Option<String>,
    end_column: Option<String>,
}

fn load_ref_measures(conn: &Connection, paths: &Paths) -> Result<Vec<Mapping>> {
    let cfg = paths.config.join("measures.csv");
    conn.execute_batch(&format!(
        r#"
        DELETE FROM ref_measures;
        INSERT INTO ref_measures
        SELECT trim(dataset), trim(dataset_id), trim(mode), trim(ccn_column),
               nullif(trim(measure_id_columns), ''), nullif(trim(measure_id_literal), ''),
               nullif(trim(measure_name_column), ''), nullif(trim(measure_name_literal), ''),
               trim(value_columns), nullif(trim(denominator_column), ''), nullif(trim(compared_column), ''),
               nullif(trim(footnote_column), ''), nullif(trim(start_column), ''), nullif(trim(end_column), ''),
               notes
        FROM read_csv({p}, header=true, all_varchar=true);
        "#,
        p = lit(&cfg.to_string_lossy())
    ))?;
    let mut stmt = conn.prepare(
        "SELECT dataset, mode, ccn_column, measure_id_columns, measure_id_literal, measure_name_column,
                measure_name_literal, value_columns, denominator_column, compared_column, footnote_column,
                start_column, end_column FROM ref_measures",
    )?;
    let split = |s: Option<String>| -> Vec<String> {
        s.map(|s| s.split('|').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
            .unwrap_or_default()
    };
    let rows = stmt.query_map([], |r| {
        Ok(Mapping {
            dataset: r.get(0)?,
            mode: r.get(1)?,
            ccn_column: r.get(2)?,
            measure_id_columns: split(r.get::<_, Option<String>>(3)?),
            measure_id_literal: r.get(4)?,
            measure_name_column: r.get(5)?,
            measure_name_literal: r.get(6)?,
            value_columns: split(r.get::<_, Option<String>>(7)?),
            denominator_column: r.get(8)?,
            compared_column: r.get(9)?,
            footnote_column: r.get(10)?,
            start_column: r.get(11)?,
            end_column: r.get(12)?,
        })
    })?;
    let mut out = Vec::new();
    for m in rows {
        let m = m?;
        ensure!(
            m.mode == "long" || m.mode == "wide",
            "measures.csv: dataset {} has unknown mode {:?}",
            m.dataset,
            m.mode
        );
        ensure!(!m.value_columns.is_empty(), "measures.csv: dataset {} has no value_columns", m.dataset);
        ensure!(
            m.measure_id_literal.is_some() || !m.measure_id_columns.is_empty(),
            "measures.csv: dataset {} needs measure_id_columns or measure_id_literal",
            m.dataset
        );
        out.push(m);
    }
    Ok(out)
}

fn qcol(c: &str) -> String {
    format!("\"{}\"", c.replace('"', "\"\""))
}
fn txt(c: &str) -> String {
    format!("nullif(trim({}), '')", qcol(c))
}
fn opt_txt(c: &Option<String>) -> String {
    c.as_deref().map(txt).unwrap_or_else(|| "NULL".into())
}
fn opt_date(c: &Option<String>) -> String {
    c.as_deref()
        .map(|c| format!("try_strptime({}, '%m/%d/%Y')::DATE", txt(c)))
        .unwrap_or_else(|| "NULL".into())
}
/// A raw cell with the catalog's "no value" spellings blanked out.
fn clean(c: &str) -> String {
    format!(
        "nullif(nullif(nullif(nullif(nullif({t}, 'Not Available'), 'Not Applicable'), 'N/A'), 'Too Few to Report'), 'Not Applicable ')",
        t = txt(c)
    )
}

fn insert_sql(m: &Mapping, path: &str, file_id: &str) -> String {
    let measure_id = match &m.measure_id_literal {
        Some(l) => lit(l),
        None => format!(
            "concat_ws(' / ', {})",
            m.measure_id_columns.iter().map(|c| txt(c)).collect::<Vec<_>>().join(", ")
        ),
    };
    let measure_name = match (&m.measure_name_literal, &m.measure_name_column) {
        (Some(l), _) => lit(l),
        (None, Some(c)) => txt(c),
        (None, None) => "NULL".into(),
    };
    let value_raw = format!(
        "coalesce({})",
        m.value_columns.iter().map(|c| clean(c)).collect::<Vec<_>>().join(", ")
    );
    format!(
        r#"
        INSERT INTO stg_cms_measures
        SELECT ccn, dataset, measure_id, measure_name, value_raw,
               TRY_CAST(replace(replace(value_raw, '%', ''), ',', '') AS DOUBLE) AS value,
               denominator, compared_to_national, footnote, start_date, end_date, source_file_id
        FROM (
            SELECT trim({ccn})       AS ccn,
                   {dataset}         AS dataset,
                   {measure_id}      AS measure_id,
                   {measure_name}    AS measure_name,
                   {value_raw}       AS value_raw,
                   {denominator}     AS denominator,
                   {compared}        AS compared_to_national,
                   {footnote}        AS footnote,
                   {start}           AS start_date,
                   {end}             AS end_date,
                   {fid}             AS source_file_id
            FROM read_csv({p}, header=true, all_varchar=true)
        )
        WHERE ccn IS NOT NULL AND ccn <> '' AND measure_id IS NOT NULL;
        "#,
        ccn = qcol(&m.ccn_column),
        dataset = lit(&m.dataset),
        denominator = opt_txt(&m.denominator_column),
        compared = opt_txt(&m.compared_column),
        footnote = opt_txt(&m.footnote_column),
        start = opt_date(&m.start_column),
        end = opt_date(&m.end_column),
        fid = lit(file_id),
        p = lit(path),
    )
}

pub fn normalize(conn: &Connection, paths: &Paths, files: &[RawFile]) -> Result<()> {
    let mappings = load_ref_measures(conn, paths)?;
    // newest file per dataset
    let mut newest: HashMap<String, &RawFile> = HashMap::new();
    for f in files {
        if let Some(ds) = dataset_of(f) {
            let e = newest.entry(ds).or_insert(f);
            if f.downloaded_at > e.downloaded_at {
                *e = f;
            }
        }
    }
    conn.execute_batch("DELETE FROM stg_cms_measures")?;
    let mut per_dataset: Vec<(String, i64)> = Vec::new();
    for (ds, file) in &newest {
        let maps: Vec<&Mapping> = mappings.iter().filter(|m| &m.dataset == ds).collect();
        if maps.is_empty() {
            tracing::warn!(dataset = ds, "no mapping in config/measures.csv; skipped");
            continue;
        }
        let path = paths.root.join(&file.path);
        let required: Vec<&str> = maps
            .iter()
            .flat_map(|m| {
                std::iter::once(m.ccn_column.as_str())
                    .chain(m.measure_id_columns.iter().map(String::as_str))
                    .chain(m.value_columns.iter().map(String::as_str))
            })
            .collect();
        check_csv_header(&path, &required)
            .with_context(|| format!("measures dataset {ds}"))?;
        let before = count(conn, "SELECT count(*) FROM stg_cms_measures")?;
        for m in maps {
            conn.execute_batch(&insert_sql(m, &path.to_string_lossy(), &file.id))?;
        }
        let after = count(conn, "SELECT count(*) FROM stg_cms_measures")?;
        per_dataset.push((ds.clone(), after - before));
    }
    per_dataset.sort();
    let n = count(conn, "SELECT count(*) FROM stg_cms_measures")?;
    info!(rows = n, per_dataset = ?per_dataset, "stg_cms_measures loaded");
    Ok(())
}

pub fn derive(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        DELETE FROM hospital_measures;
        INSERT INTO hospital_measures
        SELECT ccn, dataset, measure_id, measure_name, value_raw, value, denominator,
               compared_to_national, footnote, start_date, end_date, source_file_id
        FROM stg_cms_measures
        QUALIFY row_number() OVER (PARTITION BY ccn, dataset, measure_id
                                   ORDER BY value IS NULL, end_date DESC NULLS LAST) = 1;
        "#,
    )?;
    let n = count(conn, "SELECT count(*) FROM hospital_measures")?;
    let dup = count(
        conn,
        "SELECT count(*) - count(DISTINCT (ccn, dataset, measure_id)) FROM hospital_measures",
    )?;
    ensure!(dup == 0, "hospital_measures has {dup} duplicate keys");
    let dropped = count(conn, "SELECT count(*) FROM stg_cms_measures")? - n;
    let mut stmt = conn.prepare(
        "SELECT dataset, count(*), count(DISTINCT ccn),
                count(DISTINCT ccn) FILTER (WHERE ccn IN (SELECT ccn FROM hospitals))
         FROM hospital_measures GROUP BY 1 ORDER BY 1",
    )?;
    let cov: Vec<String> = stmt
        .query_map([], |r| {
            Ok(format!(
                "{}:{} rows/{} ccn/{} in hospitals",
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?
            ))
        })?
        .collect::<std::result::Result<_, _>>()?;
    info!(rows = n, duplicates_dropped = dropped, coverage = ?cov, "hospital_measures derived");
    Ok(())
}
