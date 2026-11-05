//! ONC — Certified Health IT Reported by Hospitals for Promoting Interoperability Performance.
//!
//! Grain of the file: one row per hospital × year × CHPL product listed under the
//! hospital's CEHRT ID. Hospitals that were in the PI file but reported no CEHRT ID
//! appear once with blank CEHRT/CHPL fields.

use crate::config::Paths;
use crate::db::lit;
use crate::download::{check_csv_header, download, latest_from_manifest, DownloadRequest};
use anyhow::Result;
use duckdb::Connection;
use ehr_atlas_core::manifest::RawFile;
use ehr_atlas_core::sources::{SourceId, ONC_LINKAGE_CSV, ONC_LINKAGE_REQUIRED_COLUMNS};
use tracing::info;

pub const SOURCE: &str = "onc_linkage";

pub async fn fetch(paths: &Paths, client: &reqwest::Client, offline: bool) -> Result<RawFile> {
    if offline {
        return latest_from_manifest(paths, SOURCE);
    }
    download(
        paths,
        client,
        DownloadRequest {
            source: SourceId::OncLinkage.as_str(),
            url: ONC_LINKAGE_CSV,
            file_name: "hospital-promoting-interoperability-chpl-linkage.csv",
            upstream_modified: None,
            notes: Some("ONC dataset page: https://healthit.gov/data/datasets/certified-health-information-technology-reported-by-hospitals-for-promoting-interoperability-performance/".into()),
        },
    )
    .await
}

pub fn normalize(conn: &Connection, paths: &Paths, file: &RawFile) -> Result<()> {
    let path = paths.root.join(&file.path);
    check_csv_header(&path, ONC_LINKAGE_REQUIRED_COLUMNS)?;
    conn.execute_batch(&format!(
        r#"
        DELETE FROM stg_onc_linkage;
        INSERT INTO stg_onc_linkage
        SELECT
            trim("Facility.ID")                                   AS ccn,
            nullif(trim("Facility.Name"), '')                     AS facility_name,
            nullif(trim("Address"), '')                           AS address,
            nullif(trim("City.Town"), '')                         AS city,
            nullif(trim("State"), '')                             AS state,
            nullif(trim("ZIP.Code"), '')                          AS zip,
            nullif(trim("County.Parish"), '')                     AS county,
            nullif(trim("Telephone.Number"), '')                  AS phone,
            CASE upper(trim("Meets.criteria.for.promoting.interoperability.of.EHRs")) WHEN 'Y' THEN TRUE WHEN 'N' THEN FALSE END AS meets_pi_criteria,
            TRY_CAST(strptime(nullif(trim("Start.Date"), ''), '%m/%d/%Y') AS DATE) AS period_start,
            TRY_CAST(strptime(nullif(trim("End.Date"), ''), '%m/%d/%Y') AS DATE)   AS period_end,
            CAST("year" AS SMALLINT)                              AS year,
            nullif(trim("CEHRT.ID"), '')                          AS cehrt_id_raw,
            CASE WHEN regexp_matches(upper(trim("CEHRT.ID")), '^[0O]{{2}}15[0-9A-Z]{{11}}$')
                 THEN '00' || substr(upper(trim("CEHRT.ID")), 3)
                 ELSE NULL END AS cehrt_id,
            nullif(trim("chpl_id"), '')                           AS chpl_id,
            nullif(trim("product_database_id"), '')               AS chpl_product_db_id,
            nullif(trim("developer_name"), '')                    AS developer_name_raw,
            nullif(trim("product_name"), '')                      AS product_name_raw,
            {fid} AS source_file_id
        FROM read_csv({p}, header=true, all_varchar=true);
        "#,
        p = lit(&path.to_string_lossy()),
        fid = lit(&file.id),
    ))?;
    let n = crate::db::count(conn, "SELECT count(*) FROM stg_onc_linkage")?;
    let years: Vec<String> = conn
        .prepare("SELECT year || ':' || count(DISTINCT ccn) FROM stg_onc_linkage GROUP BY year ORDER BY year")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    info!(rows = n, hospitals_by_year = ?years, "stg_onc_linkage loaded");
    Ok(())
}
