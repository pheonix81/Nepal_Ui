//! data.cms.gov — Medicare Inpatient Hospitals by Provider (one file per calendar year).
//!
//! Medicare fee-for-service utilization, payments and beneficiary mix per hospital, plus
//! the RUCA rurality code CMS attaches to the provider address. Newest three years.

use crate::config::Paths;
use crate::db::{count, lit};
use crate::download::{
    all_from_manifest, check_csv_header, data_cms_gov_distributions, download, DownloadRequest,
};
use anyhow::{ensure, Result};
use duckdb::Connection;
use ehr_atlas_core::manifest::RawFile;
use ehr_atlas_core::sources::{SourceId, CMS_INPATIENT_DATASET_UUID, CMS_INPATIENT_REQUIRED_COLUMNS};
use tracing::info;

pub const SOURCE: &str = "cms_inpatient";
const YEARS: usize = 3;

pub async fn fetch(paths: &Paths, client: &reqwest::Client, offline: bool) -> Result<Vec<RawFile>> {
    if offline {
        return all_from_manifest(paths, SOURCE);
    }
    let dists = data_cms_gov_distributions(client, CMS_INPATIENT_DATASET_UUID).await?;
    let mut out = Vec::new();
    for d in dists.iter().take(YEARS) {
        let year = &d.temporal[..4];
        out.push(
            download(
                paths,
                client,
                DownloadRequest {
                    source: SourceId::CmsInpatient.as_str(),
                    url: &d.download_url,
                    file_name: &format!("Medicare_Inpatient_by_Provider_{year}.csv"),
                    upstream_modified: d.modified.clone(),
                    notes: Some(format!("data.cms.gov Medicare Inpatient Hospitals by Provider, {}", d.title)),
                },
            )
            .await?,
        );
    }
    Ok(out)
}

/// Some releases carry a handful of non-UTF-8 bytes in provider names (the 2023 file
/// does), which makes DuckDB refuse the whole file. Return a path to a valid-UTF-8 copy
/// under `data/cache/cms_inpatient/` when needed, else the original path.
fn utf8_path(paths: &Paths, path: &std::path::Path) -> Result<std::path::PathBuf> {
    let bytes = std::fs::read(path)?;
    if std::str::from_utf8(&bytes).is_ok() {
        return Ok(path.to_path_buf());
    }
    let dir = paths.cache.join(SOURCE);
    std::fs::create_dir_all(&dir)?;
    let out = dir.join(path.file_name().unwrap_or_default());
    std::fs::write(&out, String::from_utf8_lossy(&bytes).as_bytes())?;
    tracing::warn!(file = %path.display(), "not valid UTF-8; loading a lossy copy from cache");
    Ok(out)
}

fn year_of(file: &RawFile) -> Option<i32> {
    file.path
        .file_stem()?
        .to_str()?
        .rsplit('_')
        .next()?
        .parse()
        .ok()
}

pub fn normalize(conn: &Connection, paths: &Paths, files: &[RawFile]) -> Result<()> {
    conn.execute_batch("DELETE FROM stg_cms_inpatient")?;
    for file in files {
        let path = utf8_path(paths, &paths.root.join(&file.path))?;
        check_csv_header(&path, CMS_INPATIENT_REQUIRED_COLUMNS)?;
        let year = year_of(file)
            .ok_or_else(|| anyhow::anyhow!("cannot read year from file name {}", file.path.display()))?;
        conn.execute_batch(&format!(
            r#"
            INSERT INTO stg_cms_inpatient
            SELECT
                trim(Rndrng_Prvdr_CCN)                        AS ccn,
                {year}                                        AS year,
                nullif(trim(Rndrng_Prvdr_Org_Name), '')       AS org_name,
                nullif(trim(Rndrng_Prvdr_City), '')           AS city,
                nullif(trim(Rndrng_Prvdr_State_Abrvtn), '')   AS state,
                nullif(trim(Rndrng_Prvdr_Zip5), '')           AS zip,
                nullif(trim(Rndrng_Prvdr_RUCA), '')           AS ruca,
                nullif(trim(Rndrng_Prvdr_RUCA_Desc), '')      AS ruca_desc,
                TRY_CAST(nullif(trim(Tot_Benes), '') AS INTEGER)           AS tot_benes,
                TRY_CAST(nullif(trim(Tot_Dschrgs), '') AS INTEGER)         AS tot_dschrgs,
                TRY_CAST(nullif(trim(Tot_Cvrd_Days), '') AS INTEGER)       AS tot_cvrd_days,
                TRY_CAST(nullif(trim(Tot_Days), '') AS INTEGER)            AS tot_days,
                TRY_CAST(nullif(trim(Tot_Submtd_Cvrd_Chrg), '') AS DOUBLE) AS tot_submtd_cvrd_chrg,
                TRY_CAST(nullif(trim(Tot_Pymt_Amt), '') AS DOUBLE)         AS tot_pymt_amt,
                TRY_CAST(nullif(trim(Tot_Mdcr_Pymt_Amt), '') AS DOUBLE)    AS tot_mdcr_pymt_amt,
                TRY_CAST(nullif(trim(Bene_Avg_Age), '') AS DOUBLE)         AS bene_avg_age,
                TRY_CAST(nullif(trim(Bene_Age_LT_65_Cnt), '') AS INTEGER)  AS bene_age_lt_65,
                TRY_CAST(nullif(trim(Bene_Age_65_74_Cnt), '') AS INTEGER)  AS bene_age_65_74,
                TRY_CAST(nullif(trim(Bene_Age_75_84_Cnt), '') AS INTEGER)  AS bene_age_75_84,
                TRY_CAST(nullif(trim(Bene_Age_GT_84_Cnt), '') AS INTEGER)  AS bene_age_gt_84,
                TRY_CAST(nullif(trim(Bene_Feml_Cnt), '') AS INTEGER)       AS bene_female,
                TRY_CAST(nullif(trim(Bene_Male_Cnt), '') AS INTEGER)       AS bene_male,
                TRY_CAST(nullif(trim(Bene_Race_Wht_Cnt), '') AS INTEGER)   AS bene_race_white,
                TRY_CAST(nullif(trim(Bene_Race_Black_Cnt), '') AS INTEGER) AS bene_race_black,
                TRY_CAST(nullif(trim(Bene_Race_API_Cnt), '') AS INTEGER)   AS bene_race_api,
                TRY_CAST(nullif(trim(Bene_Race_Hspnc_Cnt), '') AS INTEGER) AS bene_race_hispanic,
                TRY_CAST(nullif(trim(Bene_Race_NatInd_Cnt), '') AS INTEGER) AS bene_race_natind,
                TRY_CAST(nullif(trim(Bene_Race_Othr_Cnt), '') AS INTEGER)  AS bene_race_other,
                TRY_CAST(nullif(trim(Bene_Dual_Cnt), '') AS INTEGER)       AS bene_dual,
                {fid}                                         AS source_file_id
            FROM read_csv({p}, header=true, all_varchar=true)
            WHERE trim(Rndrng_Prvdr_CCN) <> '';
            "#,
            p = lit(&path.to_string_lossy()),
            fid = lit(&file.id),
        ))?;
    }
    let n = count(conn, "SELECT count(*) FROM stg_cms_inpatient")?;
    let dup = count(
        conn,
        "SELECT count(*) - count(DISTINCT (ccn, year)) FROM stg_cms_inpatient",
    )?;
    info!(rows = n, duplicate_keys = dup, "stg_cms_inpatient loaded");
    ensure!(dup == 0, "stg_cms_inpatient has {dup} duplicate (ccn, year) rows");
    Ok(())
}

pub fn derive(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        DELETE FROM hospital_medicare_inpatient;
        INSERT INTO hospital_medicare_inpatient
        SELECT ccn, year, ruca, ruca_desc, tot_benes, tot_dschrgs, tot_cvrd_days, tot_days,
               tot_submtd_cvrd_chrg, tot_pymt_amt, tot_mdcr_pymt_amt, bene_avg_age,
               bene_age_lt_65, bene_age_65_74, bene_age_75_84, bene_age_gt_84,
               bene_female, bene_male, bene_race_white, bene_race_black, bene_race_api,
               bene_race_hispanic, bene_race_natind, bene_race_other, bene_dual, source_file_id
        FROM stg_cms_inpatient;
        "#,
    )?;
    let n = count(conn, "SELECT count(*) FROM hospital_medicare_inpatient")?;
    let matched = count(
        conn,
        "SELECT count(DISTINCT ccn) FROM hospital_medicare_inpatient WHERE ccn IN (SELECT ccn FROM hospitals)",
    )?;
    info!(rows = n, hospitals_matched = matched, "hospital_medicare_inpatient derived");
    Ok(())
}
