//! CMS Provider Data Catalog — Hospital General Information (master hospital list).

use crate::config::Paths;
use crate::db::lit;
use crate::download::{
    check_csv_header, cms_pdc_distribution, download, latest_from_manifest, DownloadRequest,
};
use anyhow::Result;
use duckdb::Connection;
use ehr_atlas_core::manifest::RawFile;
use ehr_atlas_core::sources::{SourceId, CMS_HOSPITALS_DATASET_ID, CMS_HOSPITALS_REQUIRED_COLUMNS};
use tracing::info;

pub const SOURCE: &str = "cms_hospitals";

pub async fn fetch(paths: &Paths, client: &reqwest::Client, offline: bool) -> Result<RawFile> {
    if offline {
        return latest_from_manifest(paths, SOURCE);
    }
    let (url, modified, title) = cms_pdc_distribution(client, CMS_HOSPITALS_DATASET_ID).await?;
    download(
        paths,
        client,
        DownloadRequest {
            source: SourceId::CmsHospitals.as_str(),
            url: &url,
            file_name: "Hospital_General_Information.csv",
            upstream_modified: modified,
            notes: Some(format!(
                "CMS PDC dataset {CMS_HOSPITALS_DATASET_ID}: {title}"
            )),
        },
    )
    .await
}

pub fn normalize(conn: &Connection, paths: &Paths, file: &RawFile) -> Result<()> {
    let path = paths.root.join(&file.path);
    check_csv_header(&path, CMS_HOSPITALS_REQUIRED_COLUMNS)?;
    conn.execute_batch(&format!(
        r#"
        DELETE FROM stg_cms_hospitals;
        INSERT INTO stg_cms_hospitals
        SELECT
            trim("Facility ID")                       AS ccn,
            trim("Facility Name")                     AS name,
            nullif(trim("Address"), '')               AS address,
            nullif(trim("City/Town"), '')             AS city,
            nullif(trim("State"), '')                 AS state,
            nullif(trim("ZIP Code"), '')              AS zip,
            nullif(trim("County/Parish"), '')         AS county,
            nullif(trim("Telephone Number"), '')      AS phone,
            nullif(trim("Hospital Type"), '')         AS hospital_type,
            nullif(trim("Hospital Ownership"), '')    AS ownership,
            CASE upper(trim("Emergency Services")) WHEN 'YES' THEN TRUE WHEN 'NO' THEN FALSE END AS emergency_services,
            CASE upper(trim("Meets criteria for birthing friendly designation")) WHEN 'Y' THEN TRUE WHEN 'N' THEN FALSE END AS birthing_friendly,
            TRY_CAST(nullif(trim("Hospital overall rating"), 'Not Available') AS SMALLINT) AS overall_rating,
            {fid} AS source_file_id
        FROM read_csv({p}, header=true, all_varchar=true);
        "#,
        p = lit(&path.to_string_lossy()),
        fid = lit(&file.id),
    ))?;
    let n = crate::db::count(conn, "SELECT count(*) FROM stg_cms_hospitals")?;
    let dup = crate::db::count(
        conn,
        "SELECT count(*) - count(DISTINCT ccn) FROM stg_cms_hospitals",
    )?;
    info!(rows = n, duplicate_ccn = dup, "stg_cms_hospitals loaded");
    anyhow::ensure!(dup == 0, "CMS hospital file has {dup} duplicate CCNs");
    Ok(())
}
