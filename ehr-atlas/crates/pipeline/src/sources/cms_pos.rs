//! data.cms.gov — Provider of Services File (QIES), hospitals only.
//!
//! Quarterly, 473 columns covering every certified provider type. We keep the hospital
//! rows (`PRVDR_CTGRY_CD = '01'`) and the columns that describe size, control,
//! accreditation, urban/rural setting and termination. `hospital_pos` is the latest
//! quarter per CCN.

use crate::config::Paths;
use crate::db::{count, lit};
use crate::download::{
    all_from_manifest, check_csv_header, data_cms_gov_distributions, download, utf8_path,
    DownloadRequest,
};
use anyhow::{ensure, Context, Result};
use duckdb::Connection;
use ehr_atlas_core::manifest::RawFile;
use ehr_atlas_core::sources::{SourceId, CMS_POS_DATASET_UUID, CMS_POS_REQUIRED_COLUMNS};
use tracing::info;

pub const SOURCE: &str = "cms_pos";

/// `2026-04-01/2026-06-30` → `2026Q2`.
fn quarter_label(temporal: &str) -> Option<String> {
    let end = temporal.split('/').nth(1)?;
    let mut it = end.split('-');
    let y = it.next()?;
    let m: u32 = it.next()?.parse().ok()?;
    Some(format!("{y}Q{}", (m - 1) / 3 + 1))
}

pub async fn fetch(paths: &Paths, client: &reqwest::Client, offline: bool) -> Result<Vec<RawFile>> {
    if offline {
        return all_from_manifest(paths, SOURCE);
    }
    let dists = data_cms_gov_distributions(client, CMS_POS_DATASET_UUID).await?;
    let d = dists.first().context("no POS distribution")?;
    let quarter = quarter_label(&d.temporal).unwrap_or_else(|| "unknown".into());
    let file = download(
        paths,
        client,
        DownloadRequest {
            source: SourceId::CmsPos.as_str(),
            url: &d.download_url,
            file_name: &format!("POS_Hospital_and_other_{quarter}.csv"),
            upstream_modified: d.modified.clone(),
            notes: Some(format!("data.cms.gov POS QIES {quarter}: {}", d.title)),
        },
    )
    .await?;
    Ok(vec![file])
}

/// Quarter label recovered from the stored file name (`..._2026Q2.csv`).
fn quarter_of(file: &RawFile) -> String {
    file.path
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.rsplit('_').next())
        .filter(|q| q.len() == 6 && q.contains('Q'))
        .map(str::to_string)
        .unwrap_or_else(|| "unknown".into())
}

pub fn normalize(conn: &Connection, paths: &Paths, files: &[RawFile]) -> Result<()> {
    conn.execute_batch("DELETE FROM stg_cms_pos")?;
    for file in files {
        // The quarterly file carries a few non-UTF-8 bytes (0xA0).
        let path = utf8_path(paths, SOURCE, &paths.root.join(&file.path))?;
        check_csv_header(&path, CMS_POS_REQUIRED_COLUMNS)?;
        let quarter = quarter_of(file);
        conn.execute_batch(&format!(
            r#"
            INSERT INTO stg_cms_pos
            SELECT
                trim(PRVDR_NUM)                                   AS ccn,
                {q}                                               AS quarter,
                nullif(trim(PRVDR_CTGRY_CD), '')                  AS category_code,
                nullif(trim(PRVDR_CTGRY_SBTYP_CD), '')            AS subtype_code,
                nullif(trim(FAC_NAME), '')                        AS name,
                nullif(trim(ST_ADR), '')                          AS address,
                nullif(trim(CITY_NAME), '')                       AS city,
                nullif(trim(STATE_CD), '')                        AS state,
                nullif(trim(ZIP_CD), '')                          AS zip,
                nullif(trim(FIPS_STATE_CD), '')                   AS fips_state,
                nullif(trim(FIPS_CNTY_CD), '')                    AS fips_county,
                nullif(trim(CBSA_CD), '')                         AS cbsa_code,
                nullif(trim(CBSA_URBN_RRL_IND), '')               AS cbsa_urban_rural,
                nullif(trim(GNRL_CNTL_TYPE_CD), '')               AS general_control_type,
                try_strptime(nullif(trim(CRTFCTN_DT), ''), '%Y%m%d')::DATE        AS certification_date,
                try_strptime(nullif(trim(ORGNL_PRTCPTN_DT), ''), '%Y%m%d')::DATE  AS original_participation,
                nullif(trim(PGM_TRMNTN_CD), '')                   AS termination_code,
                try_strptime(nullif(trim(TRMNTN_EXPRTN_DT), ''), '%Y%m%d')::DATE  AS termination_date,
                nullif(trim(ACRDTN_TYPE_CD), '')                  AS accreditation_type,
                try_strptime(nullif(trim(ACRDTN_EFCTV_DT), ''), '%Y%m%d')::DATE   AS accreditation_effective,
                try_strptime(nullif(trim(ACRDTN_EXPRTN_DT), ''), '%Y%m%d')::DATE  AS accreditation_expiration,
                TRY_CAST(nullif(trim(BED_CNT), '') AS INTEGER)    AS bed_count,
                TRY_CAST(nullif(trim(CRTFD_BED_CNT), '') AS INTEGER) AS certified_bed_count,
                nullif(trim(MDCL_SCHL_AFLTN_CD), '')              AS medical_school_affiliation,
                nullif(trim(INTRN_RSDNT_SRVC_CD), '')             AS intern_resident_service,
                nullif(trim(MLT_FAC_ORG_NAME), '')                AS multi_facility_org,
                TRY_CAST(nullif(trim(CHOW_CNT), '') AS INTEGER)   AS chow_count,
                try_strptime(nullif(trim(CHOW_DT), ''), '%Y%m%d')::DATE           AS chow_date,
                nullif(trim(CROSS_REF_PROVIDER_NUMBER), '')       AS cross_ref_ccn,
                {fid}                                             AS source_file_id
            FROM read_csv({p}, header=true, all_varchar=true)
            WHERE trim(PRVDR_CTGRY_CD) = '01' AND trim(PRVDR_NUM) <> '';
            "#,
            q = lit(&quarter),
            p = lit(&path.to_string_lossy()),
            fid = lit(&file.id),
        ))?;
    }
    let n = count(conn, "SELECT count(*) FROM stg_cms_pos")?;
    let dup = count(
        conn,
        "SELECT count(*) - count(DISTINCT (ccn, quarter)) FROM stg_cms_pos",
    )?;
    info!(rows = n, duplicate_keys = dup, "stg_cms_pos loaded (hospitals only)");
    ensure!(dup == 0, "stg_cms_pos has {dup} duplicate (ccn, quarter) rows");
    Ok(())
}

pub fn derive(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        DELETE FROM hospital_pos;
        INSERT INTO hospital_pos
        SELECT ccn, quarter, subtype_code, name, cbsa_code, cbsa_urban_rural, general_control_type,
               certification_date, original_participation, termination_code, termination_date,
               termination_code = '00' AS is_active,
               accreditation_type, accreditation_effective, bed_count, certified_bed_count,
               medical_school_affiliation, intern_resident_service, multi_facility_org,
               chow_count, chow_date, cross_ref_ccn, source_file_id
        FROM stg_cms_pos
        QUALIFY row_number() OVER (PARTITION BY ccn ORDER BY quarter DESC) = 1;
        "#,
    )?;
    let n = count(conn, "SELECT count(*) FROM hospital_pos")?;
    let matched = count(
        conn,
        "SELECT count(*) FROM hospitals h WHERE EXISTS (SELECT 1 FROM hospital_pos p WHERE p.ccn = h.ccn)",
    )?;
    let explained = count(
        conn,
        "SELECT count(*) FROM hospitals h JOIN hospital_pos p USING (ccn) WHERE NOT h.in_cms_hgi AND p.termination_code <> '00'",
    )?;
    info!(rows = n, hospitals_matched = matched, non_hgi_ccns_terminated = explained, "hospital_pos derived");
    Ok(())
}
