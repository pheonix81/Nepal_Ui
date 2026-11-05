//! data.cms.gov — Hospital Service Area (one file per calendar year).
//!
//! Per (hospital, ZIP of beneficiary residence): Medicare inpatient days, cases and
//! charges. Cells with fewer than 11 cases are suppressed upstream (`*`) and load as NULL.
//! Only the newest year is downloaded; the file is large.

use crate::config::Paths;
use crate::db::{count, lit};
use crate::download::{
    all_from_manifest, check_csv_header, data_cms_gov_distributions, download, DownloadRequest,
};
use anyhow::{ensure, Context, Result};
use duckdb::Connection;
use ehr_atlas_core::manifest::RawFile;
use ehr_atlas_core::sources::{SourceId, CMS_SERVICE_AREA_DATASET_UUID};
use tracing::info;

pub const SOURCE: &str = "cms_service_area";
const REQUIRED_COLUMNS: &[&str] = &[
    "MEDICARE_PROV_NUM",
    "ZIP_CD_OF_RESIDENCE",
    "TOTAL_DAYS_OF_CARE",
    "TOTAL_CHARGES",
    "TOTAL_CASES",
];

pub async fn fetch(paths: &Paths, client: &reqwest::Client, offline: bool) -> Result<Vec<RawFile>> {
    if offline {
        return all_from_manifest(paths, SOURCE);
    }
    let dists = data_cms_gov_distributions(client, CMS_SERVICE_AREA_DATASET_UUID).await?;
    let d = dists.first().context("no Hospital Service Area distribution")?;
    let year = &d.temporal[..4];
    let file = download(
        paths,
        client,
        DownloadRequest {
            source: SourceId::CmsServiceArea.as_str(),
            url: &d.download_url,
            file_name: &format!("Hospital_Service_Area_{year}.csv"),
            upstream_modified: d.modified.clone(),
            notes: Some(format!("data.cms.gov Hospital Service Area, {}", d.title)),
        },
    )
    .await?;
    Ok(vec![file])
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
    conn.execute_batch("DELETE FROM stg_cms_service_area")?;
    // Keep only the newest year per CCN if several files are registered.
    let mut files: Vec<&RawFile> = files.iter().collect();
    files.sort_by_key(|f| std::cmp::Reverse(year_of(f)));
    for file in files.iter().take(1) {
        let path = paths.root.join(&file.path);
        check_csv_header(&path, REQUIRED_COLUMNS)?;
        let year = year_of(file)
            .ok_or_else(|| anyhow::anyhow!("cannot read year from file name {}", file.path.display()))?;
        conn.execute_batch(&format!(
            r#"
            INSERT INTO stg_cms_service_area
            SELECT trim(MEDICARE_PROV_NUM)                                     AS ccn,
                   {year}                                                     AS year,
                   trim(ZIP_CD_OF_RESIDENCE)                                  AS zip,
                   TRY_CAST(nullif(trim(TOTAL_DAYS_OF_CARE), '*') AS INTEGER) AS total_days,
                   TRY_CAST(nullif(trim(TOTAL_CHARGES), '*') AS DOUBLE)       AS total_charges,
                   TRY_CAST(nullif(trim(TOTAL_CASES), '*') AS INTEGER)        AS total_cases,
                   {fid}                                                      AS source_file_id
            FROM read_csv({p}, header=true, all_varchar=true)
            WHERE trim(MEDICARE_PROV_NUM) <> '' AND trim(ZIP_CD_OF_RESIDENCE) <> ''
            QUALIFY row_number() OVER (PARTITION BY ccn, zip ORDER BY total_cases DESC NULLS LAST) = 1;
            "#,
            p = lit(&path.to_string_lossy()),
            fid = lit(&file.id),
        ))?;
    }
    let n = count(conn, "SELECT count(*) FROM stg_cms_service_area")?;
    let ccns = count(conn, "SELECT count(DISTINCT ccn) FROM stg_cms_service_area")?;
    let suppressed = count(conn, "SELECT count(*) FROM stg_cms_service_area WHERE total_cases IS NULL")?;
    info!(rows = n, ccns, suppressed_rows = suppressed, "stg_cms_service_area loaded");
    Ok(())
}

pub fn derive(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        DELETE FROM hospital_service_area;
        INSERT INTO hospital_service_area
        SELECT ccn, year, zip, total_days, total_charges, total_cases, source_file_id
        FROM stg_cms_service_area;
        "#,
    )?;
    let n = count(conn, "SELECT count(*) FROM hospital_service_area")?;
    let dup = count(
        conn,
        "SELECT count(*) - count(DISTINCT (ccn, year, zip)) FROM hospital_service_area",
    )?;
    ensure!(dup == 0, "hospital_service_area has {dup} duplicate keys");
    let matched = count(
        conn,
        "SELECT count(*) FROM hospitals h WHERE EXISTS (SELECT 1 FROM hospital_service_area s WHERE s.ccn = h.ccn)",
    )?;
    info!(rows = n, hospitals_matched = matched, "hospital_service_area derived");
    Ok(())
}
