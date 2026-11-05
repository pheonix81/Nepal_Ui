//! CMS Provider Data Catalog — Promoting Interoperability - Hospital.
//!
//! One row per hospital per reporting period with the CEHRT ID attested. Newer releases
//! of this file are the signal for years ONC has not yet linked to CHPL. The reporting
//! `year` is derived from the period end date.

use crate::config::Paths;
use crate::db::lit;
use crate::download::{
    check_csv_header, cms_pdc_distribution, download, latest_from_manifest, DownloadRequest,
};
use anyhow::Result;
use duckdb::Connection;
use ehr_atlas_core::manifest::RawFile;
use ehr_atlas_core::sources::{SourceId, CMS_PI_HOSPITAL_DATASET_ID, CMS_PI_REQUIRED_COLUMNS};
use tracing::info;

pub const SOURCE: &str = "cms_pi";

pub async fn fetch(paths: &Paths, client: &reqwest::Client, offline: bool) -> Result<RawFile> {
    if offline {
        return latest_from_manifest(paths, SOURCE);
    }
    let (url, modified, title) = cms_pdc_distribution(client, CMS_PI_HOSPITAL_DATASET_ID).await?;
    download(
        paths,
        client,
        DownloadRequest {
            source: SourceId::CmsPi.as_str(),
            url: &url,
            file_name: "Promoting_Interoperability-Hospital.csv",
            upstream_modified: modified,
            notes: Some(format!(
                "CMS PDC dataset {CMS_PI_HOSPITAL_DATASET_ID}: {title}"
            )),
        },
    )
    .await
}

/// Load every CMS PI file recorded in the manifest (older releases may cover other
/// years), so the staging table accumulates all reporting periods seen so far.
pub fn normalize_all(conn: &Connection, paths: &Paths, files: &[RawFile]) -> Result<()> {
    conn.execute_batch("DELETE FROM stg_cms_pi")?;
    for file in files {
        let path = paths.root.join(&file.path);
        check_csv_header(&path, CMS_PI_REQUIRED_COLUMNS)?;
        conn.execute_batch(&format!(
            r#"
            INSERT INTO stg_cms_pi
            SELECT
                trim("Facility ID")                                   AS ccn,
                nullif(trim("Facility Name"), '')                     AS facility_name,
                nullif(trim("Address"), '')                           AS address,
                nullif(trim("City/Town"), '')                         AS city,
                nullif(trim("State"), '')                             AS state,
                nullif(trim("ZIP Code"), '')                          AS zip,
                nullif(trim("County/Parish"), '')                     AS county,
                nullif(trim("Telephone Number"), '')                  AS phone,
                CASE upper(trim("Meets criteria for promoting interoperability of EHRs")) WHEN 'Y' THEN TRUE WHEN 'N' THEN FALSE END AS meets_pi_criteria,
                TRY_CAST(strptime(nullif(trim("Start Date"), ''), '%m/%d/%Y') AS DATE) AS period_start,
                TRY_CAST(strptime(nullif(trim("End Date"), ''), '%m/%d/%Y') AS DATE)   AS period_end,
                CAST(year(TRY_CAST(strptime(nullif(trim("End Date"), ''), '%m/%d/%Y') AS DATE)) AS SMALLINT) AS year,
                nullif(trim("CEHRT ID"), '')                          AS cehrt_id_raw,
                CASE WHEN regexp_matches(upper(trim("CEHRT ID")), '^[0O]{{2}}15[0-9A-Z]{{11}}$')
                 THEN '00' || substr(upper(trim("CEHRT ID")), 3)
                 ELSE NULL END AS cehrt_id,
                {fid} AS source_file_id
            FROM read_csv({p}, header=true, all_varchar=true);
            "#,
            p = lit(&path.to_string_lossy()),
            fid = lit(&file.id),
        ))?;
    }
    // Same (ccn, year, cehrt_id) can appear in two releases: keep the newest file.
    conn.execute_batch(
        r#"
        CREATE OR REPLACE TEMP TABLE _pi_dedup AS
        SELECT p.* FROM stg_cms_pi p
        JOIN source_files f ON f.id = p.source_file_id
        QUALIFY row_number() OVER (PARTITION BY p.ccn, p.year, p.cehrt_id ORDER BY f.downloaded_at DESC) = 1;
        DELETE FROM stg_cms_pi;
        INSERT INTO stg_cms_pi SELECT * FROM _pi_dedup;
        DROP TABLE _pi_dedup;
        "#,
    )?;
    let n = crate::db::count(conn, "SELECT count(*) FROM stg_cms_pi")?;
    let years: Vec<String> = conn
        .prepare(
            "SELECT year || ':' || count(DISTINCT ccn) FROM stg_cms_pi GROUP BY year ORDER BY year",
        )?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    info!(rows = n, hospitals_by_year = ?years, "stg_cms_pi loaded");
    Ok(())
}
