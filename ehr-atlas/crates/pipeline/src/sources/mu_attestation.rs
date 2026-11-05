//! ONC — EHR Products Used for Meaningful Use Attestation (#29).
//!
//! Medicare EHR Incentive Program attestations joined by ONC to the CHPL of the time: one
//! row per provider × program year × certified product, 2011–2016 (plus a handful of 2017
//! stragglers). Hospital rows carry the CCN, so this is the hospital vendor history before
//! the Promoting Interoperability years in `hospital_ehr` (2023+); no public hospital-level
//! source covers 2017–2022.
//!
//! Like a PI-era CEHRT ID, an attestation lists several certified products (EHR core, ED,
//! portal, eCQM, public-health interfaces …), so `derive` runs the same primary-vendor rule
//! as `build` (see `build::vendor_year_sql`) into `hospital_vendor_year_mu`. Developer names
//! are the MU-era ones (Siemens, CPSI, Healthcare Management Systems, McKesson …), mapped to
//! today's owner in `config/vendor_map.csv` so an acquisition does not read as a switch.
//! McKesson is the exception that needs the product: Paragon went to Allscripts (now Altera)
//! in 2017, Horizon Clinicals was retired, so Paragon rows are assigned to Altera here.
//!
//! The file is Windows-1252, not UTF-8 (® / ™ in product names), and DuckDB's `ignore_errors` drops a
//! varying number of rows on such input, so it is loaded through a UTF-8 copy (`utf8_path`).

use crate::build::vendor_year_sql;
use crate::config::Paths;
use crate::db::{count, lit};
use crate::download::{all_from_manifest, check_csv_header, download, utf8_path, DownloadRequest};
use anyhow::{ensure, Result};
use duckdb::Connection;
use ehr_atlas_core::manifest::RawFile;
use ehr_atlas_core::sources::{SourceId, ONC_MU_ATTESTATION_CSV, ONC_MU_ATTESTATION_REQUIRED_COLUMNS};
use tracing::info;

pub const SOURCE: &str = "mu_attestation";

pub async fn fetch(paths: &Paths, client: &reqwest::Client, offline: bool) -> Result<Vec<RawFile>> {
    if offline {
        return all_from_manifest(paths, SOURCE);
    }
    let file = download(
        paths,
        client,
        DownloadRequest {
            source: SourceId::MuAttestation.as_str(),
            url: ONC_MU_ATTESTATION_CSV,
            file_name: "MU_REPORT.csv",
            upstream_modified: None,
            notes: Some("ONC dataset page: https://healthit.gov/data/datasets/ehr-products-used-meaningful-use-attestation/".into()),
        },
    )
    .await?;
    Ok(vec![file])
}

/// Loads the newest file only: the dataset is closed (the program ended in 2016), so a
/// later download is a re-publication of the same attestations, not new years.
pub fn normalize(conn: &Connection, paths: &Paths, files: &[RawFile]) -> Result<()> {
    let Some(file) = files.iter().max_by(|a, b| a.downloaded_at.cmp(&b.downloaded_at)) else {
        anyhow::bail!("no {SOURCE} file downloaded; run without --offline first");
    };
    let path = utf8_path(paths, SOURCE, &paths.root.join(&file.path))?;
    check_csv_header(&path, ONC_MU_ATTESTATION_REQUIRED_COLUMNS)?;
    conn.execute_batch(&format!(
        r#"
        DELETE FROM stg_mu_attestation;
        INSERT INTO stg_mu_attestation
        SELECT
            trim(CCN)                                             AS ccn,
            nullif(trim(NPI), '')                                 AS npi,
            nullif(trim(Business_State_Territory), '')            AS state,
            nullif(trim(ZIP), '')                                 AS zip,
            nullif(trim(Hospital_Type), '')                       AS hospital_type,
            nullif(trim(Program_Type), '')                        AS program_type,
            CAST(trim(Program_Year) AS SMALLINT)                  AS program_year,
            nullif(trim(Provider_Stage_Number), '')               AS stage,
            TRY_CAST(nullif(trim(Payment_Year), '') AS SMALLINT)  AS payment_year,
            TRY_CAST(nullif(trim(Attestation_Year), '') AS SMALLINT)  AS attestation_year,
            TRY_CAST(nullif(trim(Attestation_Month), '') AS SMALLINT) AS attestation_month,
            upper(trim(EHR_Certification_Number))                 AS cehrt_id,
            upper(trim(EHR_Product_CHP_Id))                       AS chp_id,
            nullif(trim(Vendor_Name), '')                         AS developer_name_raw,
            nullif(trim(EHR_Product_Name), '')                    AS product_name_raw,
            nullif(trim(EHR_Product_Version), '')                 AS product_version,
            nullif(trim(Product_Classification), '')              AS product_classification,
            nullif(trim(Product_Setting), '')                     AS product_setting,
            nullif(trim(Product_Certification_Edition_Yr), '')    AS edition,
            {fid}                                                 AS source_file_id
        FROM read_csv({p}, header=true, all_varchar=true)
        WHERE Provider_Type = 'Hospital' AND nullif(trim(CCN), '') IS NOT NULL
          AND nullif(trim(EHR_Certification_Number), '') IS NOT NULL
          AND nullif(trim(EHR_Product_CHP_Id), '') IS NOT NULL;
        "#,
        p = lit(&path.to_string_lossy()),
        fid = lit(&file.id),
    ))?;
    let n = count(conn, "SELECT count(*) FROM stg_mu_attestation")?;
    let years: Vec<String> = conn
        .prepare("SELECT program_year || ':' || count(DISTINCT ccn) FROM stg_mu_attestation GROUP BY program_year ORDER BY program_year")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    info!(rows = n, hospitals_by_year = ?years, "stg_mu_attestation loaded (hospitals only)");
    ensure!(n > 0, "stg_mu_attestation is empty; the hospital filter (Provider_Type = 'Hospital') may no longer match");
    Ok(())
}

pub fn derive(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        DELETE FROM hospital_vendor_year_mu;
        DELETE FROM hospital_ehr_mu;
        INSERT INTO hospital_ehr_mu
        SELECT ccn, year, cehrt_id, chp_id, developer_name_raw, product_name_raw, product_classification,
               vendor, vendor_tier,
               EXISTS (SELECT 1 FROM ref_core_products p
                       WHERE p.vendor = x.vendor AND x.product_name_raw ILIKE p.product_pattern) AS is_core_product,
               source_file_id
        FROM (
            SELECT a.ccn, a.program_year AS year, a.cehrt_id, a.chp_id,
                   any_value(a.developer_name_raw) AS developer_name_raw,
                   any_value(a.product_name_raw) AS product_name_raw,
                   any_value(a.product_classification) AS product_classification,
                   any_value(CASE WHEN m.vendor = 'McKesson' AND a.product_name_raw ILIKE 'Paragon%'
                                  THEN 'Altera Digital Health'
                                  ELSE coalesce(m.vendor, a.developer_name_raw) END) AS vendor,
                   any_value(CASE WHEN a.developer_name_raw IS NULL THEN NULL
                                  ELSE coalesce(m.vendor_tier, 'unmapped') END) AS vendor_tier,
                   min(a.source_file_id) AS source_file_id
            FROM stg_mu_attestation a
            LEFT JOIN ref_vendor_map m ON lower(trim(m.developer_name_raw)) = lower(trim(a.developer_name_raw))
            WHERE a.ccn IN (SELECT ccn FROM hospitals)
            GROUP BY a.ccn, a.program_year, a.cehrt_id, a.chp_id
        ) x;
        "#,
    )?;
    conn.execute_batch(&vendor_year_sql("hospital_ehr_mu", "hospital_vendor_year_mu"))?;

    let n_ehr = count(conn, "SELECT count(*) FROM hospital_ehr_mu")?;
    let n_hy = count(conn, "SELECT count(*) FROM hospital_vendor_year_mu")?;
    let n_core = count(conn, "SELECT count(*) FROM hospital_vendor_year_mu WHERE rule IN ('core_product', 'single_vendor')")?;
    let dropped = count(
        conn,
        "SELECT count(DISTINCT ccn) FROM stg_mu_attestation WHERE ccn NOT IN (SELECT ccn FROM hospitals)",
    )?;
    info!(
        hospital_ehr_mu = n_ehr,
        hospital_years = n_hy,
        decided_by_core_or_single = n_core,
        ccns_not_in_hospitals = dropped,
        "MU-era vendor history derived"
    );
    ensure!(n_hy > 0, "hospital_vendor_year_mu is empty");
    Ok(())
}
