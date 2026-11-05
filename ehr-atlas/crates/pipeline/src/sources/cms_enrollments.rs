//! data.cms.gov — Hospital Enrollments (monthly PECOS extract).
//!
//! One row per enrollment; in practice one row per CCN (units and subproviders carry
//! their own CCN and point at the parent through `CAH OR HOSPITAL CCN`, which can be a
//! `|`-separated list). Gives the CCN → NPI crosswalk, legal and DBA names, and the
//! practice-location street address used for geocoding.

use crate::config::Paths;
use crate::db::{count, lit};
use crate::download::{
    all_from_manifest, check_csv_header, data_cms_gov_distributions, download, DownloadRequest,
};
use anyhow::{ensure, Result};
use duckdb::Connection;
use ehr_atlas_core::manifest::RawFile;
use ehr_atlas_core::sources::{
    SourceId, CMS_ENROLLMENTS_DATASET_UUID, CMS_ENROLLMENTS_REQUIRED_COLUMNS,
};
use tracing::info;

pub const SOURCE: &str = "cms_enrollments";

/// Newest monthly release only; older releases stay in the manifest but are not loaded.
pub async fn fetch(paths: &Paths, client: &reqwest::Client, offline: bool) -> Result<Vec<RawFile>> {
    if offline {
        let files = all_from_manifest(paths, SOURCE)?;
        return Ok(vec![files.last().cloned().expect("non-empty")]);
    }
    let dists = data_cms_gov_distributions(client, CMS_ENROLLMENTS_DATASET_UUID).await?;
    let d = &dists[0];
    let rf = download(
        paths,
        client,
        DownloadRequest {
            source: SourceId::CmsEnrollments.as_str(),
            url: &d.download_url,
            file_name: "Hospital_Enrollments.csv",
            upstream_modified: d.modified.clone(),
            notes: Some(format!(
                "data.cms.gov dataset {CMS_ENROLLMENTS_DATASET_UUID}: {} ({})",
                d.title, d.temporal
            )),
        },
    )
    .await?;
    Ok(vec![rf])
}

pub fn normalize(conn: &Connection, paths: &Paths, files: &[RawFile]) -> Result<()> {
    let file = files.last().expect("fetch returns one file");
    let path = paths.root.join(&file.path);
    check_csv_header(&path, CMS_ENROLLMENTS_REQUIRED_COLUMNS)?;
    // The file is Latin-1 (e.g. an en dash in a DBA name). Dates are M/D/YYYY. CCNs and
    // ZIP codes lost their leading zeros upstream: CCNs are padded back to 6 chars; a ZIP
    // of ≤5 chars is a bare ZIP5, longer is ZIP+4 without the hyphen.
    conn.execute_batch(&format!(
        r#"
        DELETE FROM stg_cms_enrollments;
        INSERT INTO stg_cms_enrollments
        SELECT
            trim("ENROLLMENT ID")                                   AS enrollment_id,
            nullif(trim("ENROLLMENT STATE"), '')                    AS enrollment_state,
            nullif(trim("PROVIDER TYPE CODE"), '')                  AS provider_type_code,
            nullif(trim("PROVIDER TYPE TEXT"), '')                  AS provider_type_text,
            trim("NPI")                                             AS npi,
            CASE upper(trim("MULTIPLE NPI FLAG")) WHEN 'Y' THEN TRUE WHEN 'N' THEN FALSE END AS multiple_npi_flag,
            lpad(trim("CCN"), 6, '0')                              AS ccn,
            nullif(trim("ASSOCIATE ID"), '')                        AS associate_id,
            nullif(trim("ORGANIZATION NAME"), '')                   AS organization_name,
            nullif(trim("DOING BUSINESS AS NAME"), '')              AS dba_name,
            TRY_CAST(strptime(nullif(trim("INCORPORATION DATE"), ''), '%m/%d/%Y') AS DATE) AS incorporation_date,
            nullif(trim("INCORPORATION STATE"), '')                 AS incorporation_state,
            nullif(trim("ORGANIZATION TYPE STRUCTURE"), '')         AS org_type_structure,
            nullif(trim("PROPRIETARY NONPROFIT"), '')               AS proprietary_nonprofit,
            nullif(trim("ADDRESS LINE 1"), '')                      AS address1,
            nullif(trim("ADDRESS LINE 2"), '')                      AS address2,
            nullif(trim("CITY"), '')                                AS city,
            nullif(trim("STATE"), '')                               AS state,
            CASE WHEN nullif(trim("ZIP CODE"), '') IS NULL THEN NULL
                 WHEN length(trim("ZIP CODE")) <= 5 THEN lpad(trim("ZIP CODE"), 5, '0')
                 ELSE lpad(trim("ZIP CODE"), 9, '0') END            AS zip,
            nullif(trim("PRACTICE LOCATION TYPE"), '')              AS practice_location_type,
            CASE upper(trim("REH CONVERSION FLAG")) WHEN 'Y' THEN TRUE WHEN 'N' THEN FALSE END AS reh_conversion_flag,
            TRY_CAST(strptime(nullif(trim("REH CONVERSION DATE"), ''), '%m/%d/%Y') AS DATE) AS reh_conversion_date,
            -- may be a '|'-separated list; pad each entry to 6 chars
            nullif(array_to_string(list_transform(string_split(trim("CAH OR HOSPITAL CCN"), '|'), x -> lpad(trim(x), 6, '0')), '|'), '000000') AS cah_or_hospital_ccn,
            {fid} AS source_file_id
        FROM read_csv({p}, header=true, all_varchar=true, encoding='latin-1')
        WHERE trim("CCN") <> '' AND trim("NPI") <> '';
        "#,
        p = lit(&path.to_string_lossy()),
        fid = lit(&file.id),
    ))?;
    let n = count(conn, "SELECT count(*) FROM stg_cms_enrollments")?;
    let ccns = count(conn, "SELECT count(DISTINCT ccn) FROM stg_cms_enrollments")?;
    let npis = count(conn, "SELECT count(DISTINCT npi) FROM stg_cms_enrollments")?;
    info!(rows = n, distinct_ccn = ccns, distinct_npi = npis, "stg_cms_enrollments loaded");
    ensure!(n > 0, "Hospital Enrollments file loaded zero rows");
    Ok(())
}

pub fn derive(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        DELETE FROM hospital_npi;
        INSERT INTO hospital_npi
        SELECT ccn, npi,
               arg_max(organization_name, enrollment_id) AS npi_org_name,
               arg_max(dba_name, enrollment_id)          AS dba_name,
               max(enrollment_id)                        AS enrollment_id,
               'cms_enrollments'                         AS match_method,
               max(source_file_id)                       AS source_file_id
        FROM stg_cms_enrollments
        GROUP BY ccn, npi;

        DELETE FROM hospital_enrollment;
        INSERT INTO hospital_enrollment
        WITH ranked AS (
            SELECT *,
                   -- the parent CCN is the first listed value that is not the row's own CCN
                   (SELECT min(p) FROM unnest(string_split(coalesce(cah_or_hospital_ccn, ''), '|')) AS t(p)
                     WHERE trim(p) <> '' AND trim(p) <> ccn) AS parent_ccn,
                   row_number() OVER (
                       PARTITION BY ccn
                       ORDER BY (cah_or_hospital_ccn IS NULL OR cah_or_hospital_ccn = ccn) DESC,
                                (practice_location_type = 'MAIN/PRIMARY HOSPITAL LOCATION') DESC,
                                incorporation_date DESC NULLS LAST,
                                enrollment_id
                   ) AS rn,
                   count(DISTINCT npi) OVER (PARTITION BY ccn) AS n_npis
            FROM stg_cms_enrollments
        )
        SELECT ccn, enrollment_id, organization_name, dba_name, incorporation_date, incorporation_state,
               org_type_structure, proprietary_nonprofit, address1, address2, city, state, zip,
               reh_conversion_flag, reh_conversion_date, parent_ccn, n_npis, source_file_id
        FROM ranked WHERE rn = 1;
        "#,
    )?;
    let n_npi = count(conn, "SELECT count(*) FROM hospital_npi")?;
    let n_enr = count(conn, "SELECT count(*) FROM hospital_enrollment")?;
    let dup = count(conn, "SELECT count(*) - count(DISTINCT ccn) FROM hospital_enrollment")?;
    ensure!(dup == 0, "hospital_enrollment has {dup} duplicate CCNs");
    let cms = count(conn, "SELECT count(*) FROM hospitals WHERE in_cms_hgi")?;
    let covered = count(
        conn,
        "SELECT count(*) FROM hospitals h WHERE in_cms_hgi AND EXISTS (SELECT 1 FROM hospital_enrollment e WHERE e.ccn = h.ccn)",
    )?;
    let parents = count(conn, "SELECT count(*) FROM hospital_enrollment WHERE parent_ccn IS NOT NULL")?;
    info!(
        hospital_npi = n_npi,
        hospital_enrollment = n_enr,
        with_parent = parents,
        cms_hospitals = cms,
        cms_hospitals_covered = covered,
        coverage_pct = format!("{:.1}", 100.0 * covered as f64 / cms.max(1) as f64),
        "enrollment tables derived"
    );
    Ok(())
}
