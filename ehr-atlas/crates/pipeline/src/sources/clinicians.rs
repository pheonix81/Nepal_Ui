//! Clinician-side EHR signal: which certified EHR the clinicians *affiliated with* each
//! hospital reported under MIPS Promoting Interoperability.
//!
//! Two files:
//!
//! * **CMS Facility Affiliation Data** (Provider Data Catalog `27ea-46a8`, ~2.3M rows):
//!   clinician NPI → facility CCN, with a facility type. We keep `Hospital` rows
//!   (~1.9M rows, ~930k NPIs, ~4,600 hospitals). A clinician can be affiliated with
//!   several hospitals.
//! * **ONC "Certified Health IT Reported by Clinicians for Promoting Interoperability
//!   Performance"** public-use file (QPP × CHPL), one performance year in four CSV parts
//!   (~83 MB each) stored in Git LFS under `puf/<year>/` of `onc-healthit/onc-open-data`.
//!   One row per clinician × reporting entity × certified product; the same clinician often
//!   appears under several groups. Columns: provider_key, npi, grp_key,
//!   practice_state_or_us_territory, practice_size, clinician_specialty, participation_type,
//!   chpl_id, developer, product, product_id, edition.
//!
//! `fetch` takes the newest performance year available in the repo.
//!
//! Derived tables:
//! * `clinician_affiliations` (npi, ccn) — distinct hospital affiliations;
//! * `clinician_cehrt` (npi, year, chpl_id) with the canonical `vendor` via `ref_vendor_map`;
//! * `hospital_clinician_vendor` (ccn, year, vendor): distinct affiliated clinicians per
//!   core-EHR-tier vendor, share of affiliated clinicians with any core-tier vendor, rank.
//!   Rank 1 is the "clinician-side vendor" shown in `hospital_profile`.
//! * `practice_vendor` (group_key, year): the MIPS reporting entity as a practice, with its
//!   majority core-tier vendor, size, state, top specialty and how many of its clinicians
//!   also hold a hospital affiliation (office-based vs hospital-employed groups).

use crate::config::Paths;
use crate::db::{count, lit};
use crate::download::{
    all_from_manifest, check_csv_header, cms_pdc_distribution, download, github_dir,
    DownloadRequest,
};
use anyhow::{bail, ensure, Context, Result};
use duckdb::Connection;
use ehr_atlas_core::manifest::RawFile;
use ehr_atlas_core::sources::{
    SourceId, CMS_FACILITY_AFFILIATION_DATASET_ID, CMS_FACILITY_AFFILIATION_REQUIRED_COLUMNS,
    ONC_CLINICIAN_PUF_DIR, ONC_CLINICIAN_PUF_REQUIRED_COLUMNS, ONC_OPEN_DATA_MEDIA,
    ONC_OPEN_DATA_REPO,
};
use tracing::info;

pub const SOURCE: &str = "clinicians";
const AFFILIATION_SOURCE: &str = "cms_facility_affiliation";
const PUF_SOURCE: &str = "onc_clinician_cehrt";

pub async fn fetch(paths: &Paths, client: &reqwest::Client, offline: bool) -> Result<Vec<RawFile>> {
    if offline {
        let mut files = Vec::new();
        files.push(
            all_from_manifest(paths, AFFILIATION_SOURCE)?
                .pop()
                .context("no facility affiliation file in manifest")?,
        );
        files.extend(all_from_manifest(paths, PUF_SOURCE)?);
        return Ok(files);
    }
    let mut files = Vec::new();

    // (a) Facility Affiliation Data
    let (url, modified, title) =
        cms_pdc_distribution(client, CMS_FACILITY_AFFILIATION_DATASET_ID).await?;
    files.push(
        download(
            paths,
            client,
            DownloadRequest {
                source: SourceId::CmsFacilityAffiliation.as_str(),
                url: &url,
                file_name: "Facility_Affiliation.csv",
                upstream_modified: modified,
                notes: Some(format!("CMS PDC dataset {CMS_FACILITY_AFFILIATION_DATASET_ID}: {title}")),
            },
        )
        .await?,
    );

    // (b) ONC clinician CEHRT PUF, newest performance year, every part.
    let years = github_dir(client, ONC_OPEN_DATA_REPO, ONC_CLINICIAN_PUF_DIR).await?;
    let year = years
        .iter()
        .filter(|e| e.is_dir)
        .filter_map(|e| e.name.parse::<u16>().ok())
        .max()
        .context("no year directories under puf/ in onc-open-data")?;
    let dir = format!("{ONC_CLINICIAN_PUF_DIR}/{year}");
    let mut parts: Vec<_> = github_dir(client, ONC_OPEN_DATA_REPO, &dir)
        .await?
        .into_iter()
        .filter(|e| !e.is_dir && e.name.ends_with(".csv"))
        .collect();
    parts.sort_by(|a, b| a.name.cmp(&b.name));
    ensure!(!parts.is_empty(), "no CSV parts under {dir}");
    info!(year, parts = parts.len(), "ONC clinician CEHRT PUF");
    for p in parts {
        // Files are Git LFS objects: the media host serves the bytes, raw.githubusercontent
        // serves 133-byte pointers.
        let url = format!("{ONC_OPEN_DATA_MEDIA}{}", p.path);
        files.push(
            download(
                paths,
                client,
                DownloadRequest {
                    source: SourceId::OncClinicianCehrt.as_str(),
                    url: &url,
                    file_name: &p.name,
                    upstream_modified: None,
                    notes: Some(format!(
                        "ONC clinician CEHRT PUF, performance year {year}; dataset page \
                         https://healthit.gov/data/datasets/certified-health-information-technology-reported-clinicians-promoting/"
                    )),
                },
            )
            .await?,
        );
    }
    Ok(files)
}

/// Performance year from `clinician-certified-technology-pi-<year>-<part>.csv`.
fn puf_year(file: &RawFile) -> Result<u16> {
    let name = file
        .path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let caps: Vec<&str> = name.split('-').collect();
    // […, "pi", "<year>", "<part>.csv"]
    let idx = caps
        .iter()
        .position(|s| *s == "pi")
        .with_context(|| format!("unexpected PUF file name {name}"))?;
    caps.get(idx + 1)
        .and_then(|y| y.parse::<u16>().ok())
        .with_context(|| format!("cannot read the performance year from {name}"))
}

pub fn normalize(conn: &Connection, paths: &Paths, files: &[RawFile]) -> Result<()> {
    let affiliation = files
        .iter()
        .filter(|f| f.source == AFFILIATION_SOURCE)
        .max_by_key(|f| f.downloaded_at)
        .context("no facility affiliation file among the fetched files")?;
    let mut pufs: Vec<&RawFile> = files.iter().filter(|f| f.source == PUF_SOURCE).collect();
    if pufs.is_empty() {
        bail!("no ONC clinician CEHRT PUF parts among the fetched files");
    }
    pufs.sort_by_key(|f| f.downloaded_at);

    // --- affiliations (hospitals only) ---
    let path = paths.root.join(&affiliation.path);
    check_csv_header(&path, CMS_FACILITY_AFFILIATION_REQUIRED_COLUMNS)?;
    conn.execute_batch(&format!(
        r#"
        DELETE FROM stg_cms_facility_affiliation;
        INSERT INTO stg_cms_facility_affiliation
        SELECT trim("NPI")                                            AS npi,
               nullif(trim("Ind_PAC_ID"), '')                         AS pac_id,
               nullif(trim("Provider Last Name"), '')                 AS last_name,
               nullif(trim("Provider First Name"), '')                AS first_name,
               nullif(trim("facility_type"), '')                      AS facility_type,
               upper(trim("Facility Affiliations Certification Number")) AS ccn,
               {fid}                                                  AS source_file_id
        FROM read_csv({p}, header=true, all_varchar=true)
        WHERE trim("facility_type") = 'Hospital'
          AND trim("Facility Affiliations Certification Number") <> ''
          AND trim("NPI") <> '';
        "#,
        p = lit(&path.to_string_lossy()),
        fid = lit(&affiliation.id),
    ))?;
    let n_aff = count(conn, "SELECT count(*) FROM stg_cms_facility_affiliation")?;
    let n_npi = count(conn, "SELECT count(DISTINCT npi) FROM stg_cms_facility_affiliation")?;
    let n_ccn = count(conn, "SELECT count(DISTINCT ccn) FROM stg_cms_facility_affiliation")?;
    info!(rows = n_aff, clinicians = n_npi, hospitals = n_ccn, "stg_cms_facility_affiliation loaded");

    // --- clinician CEHRT PUF: newest download of each (year, part); de-dup on (npi, year, chpl_id) ---
    conn.execute_batch("DELETE FROM stg_onc_clinician_cehrt; CREATE OR REPLACE TEMP TABLE _puf_raw AS SELECT * FROM stg_onc_clinician_cehrt LIMIT 0;")?;
    let mut seen: std::collections::HashSet<String> = Default::default();
    for file in pufs.iter().rev() {
        let name = file.path.file_name().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        if !seen.insert(name.clone()) {
            continue;
        }
        let year = puf_year(file)?;
        let path = paths.root.join(&file.path);
        check_csv_header(&path, ONC_CLINICIAN_PUF_REQUIRED_COLUMNS)?;
        conn.execute_batch(&format!(
            r#"
            INSERT INTO _puf_raw
            SELECT {year}                                          AS performance_year,
                   nullif(trim("provider_key"), '')                AS provider_key,
                   trim("npi")                                     AS npi,
                   nullif(trim("grp_key"), '')                     AS group_key,
                   nullif(trim("practice_state_or_us_territory"), '') AS practice_state,
                   TRY_CAST(nullif(trim("practice_size"), '') AS INTEGER) AS practice_size,
                   nullif(trim("clinician_specialty"), '')         AS specialty,
                   nullif(trim("participation_type"), '')          AS participation_type,
                   nullif(trim("chpl_id"), '')                     AS chpl_id,
                   nullif(trim("developer"), '')                   AS developer_name_raw,
                   nullif(trim("product"), '')                     AS product_name_raw,
                   nullif(trim("product_id"), '')                  AS chpl_product_db_id,
                   nullif(trim("edition"), '')                     AS edition,
                   {fid}                                           AS source_file_id
            FROM read_csv({p}, header=true, all_varchar=true)
            WHERE trim("npi") <> '';
            "#,
            p = lit(&path.to_string_lossy()),
            fid = lit(&file.id),
        ))?;
        let n = count(conn, &format!("SELECT count(*) FROM _puf_raw WHERE source_file_id = {}", lit(&file.id)))?;
        info!(file = name, year, rows = n, "PUF part loaded");
    }
    conn.execute_batch(
        r#"
        INSERT INTO stg_onc_clinician_cehrt
        SELECT * FROM _puf_raw
        WHERE chpl_id IS NOT NULL
        QUALIFY row_number() OVER (PARTITION BY npi, performance_year, chpl_id
                                   ORDER BY participation_type = 'Group' DESC, practice_size DESC NULLS LAST, provider_key) = 1;
        DROP TABLE _puf_raw;
        "#,
    )?;
    let n = count(conn, "SELECT count(*) FROM stg_onc_clinician_cehrt")?;
    let n_npi = count(conn, "SELECT count(DISTINCT npi) FROM stg_onc_clinician_cehrt")?;
    let years: Vec<String> = conn
        .prepare("SELECT performance_year || ':' || count(DISTINCT npi) FROM stg_onc_clinician_cehrt GROUP BY performance_year ORDER BY performance_year")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    info!(rows = n, clinicians = n_npi, clinicians_by_year = ?years, "stg_onc_clinician_cehrt loaded");
    Ok(())
}

pub fn derive(conn: &Connection) -> Result<()> {
    ensure!(
        count(conn, "SELECT count(*) FROM stg_cms_facility_affiliation")? > 0,
        "stg_cms_facility_affiliation is empty; run `ehr-atlas clinicians` first"
    );
    conn.execute_batch(
        r#"
        DELETE FROM clinician_affiliations;
        INSERT INTO clinician_affiliations
        SELECT npi, ccn, any_value(facility_type), any_value(source_file_id)
        FROM stg_cms_facility_affiliation
        GROUP BY npi, ccn;

        DELETE FROM clinician_cehrt;
        INSERT INTO clinician_cehrt
        SELECT c.npi, c.performance_year, c.chpl_id, c.developer_name_raw, c.product_name_raw,
               coalesce(m.vendor, c.developer_name_raw)        AS vendor,
               CASE WHEN c.developer_name_raw IS NULL THEN NULL ELSE coalesce(m.vendor_tier, 'unmapped') END AS vendor_tier,
               c.practice_state, c.specialty, c.source_file_id
        FROM stg_onc_clinician_cehrt c
        LEFT JOIN ref_vendor_map m ON lower(trim(m.developer_name_raw)) = lower(trim(c.developer_name_raw));

        DELETE FROM hospital_clinician_vendor;
        INSERT INTO hospital_clinician_vendor
        WITH per_vendor AS (
            SELECT a.ccn, c.performance_year, c.vendor, count(DISTINCT c.npi) AS n_clinicians
            FROM clinician_affiliations a
            JOIN clinician_cehrt c ON c.npi = a.npi
            WHERE c.vendor_tier = 'core_ehr'
            GROUP BY 1, 2, 3
        ), totals AS (
            SELECT a.ccn, c.performance_year, count(DISTINCT c.npi) AS n_total
            FROM clinician_affiliations a
            JOIN clinician_cehrt c ON c.npi = a.npi
            WHERE c.vendor_tier = 'core_ehr'
            GROUP BY 1, 2
        )
        SELECT p.ccn, p.performance_year, p.vendor, p.n_clinicians,
               p.n_clinicians / t.n_total AS share,
               row_number() OVER (PARTITION BY p.ccn, p.performance_year ORDER BY p.n_clinicians DESC, p.vendor) AS rank
        FROM per_vendor p JOIN totals t USING (ccn, performance_year);

        DELETE FROM practice_vendor;
        INSERT INTO practice_vendor
        WITH rows_ AS (
            SELECT s.group_key, s.performance_year, s.npi, s.participation_type, s.practice_state,
                   s.practice_size, s.specialty,
                   coalesce(m.vendor, s.developer_name_raw) AS vendor,
                   CASE WHEN s.developer_name_raw IS NULL THEN NULL ELSE coalesce(m.vendor_tier, 'unmapped') END AS vendor_tier
            FROM stg_onc_clinician_cehrt s
            LEFT JOIN ref_vendor_map m ON lower(trim(m.developer_name_raw)) = lower(trim(s.developer_name_raw))
            WHERE s.group_key IS NOT NULL
        ), grp AS (
            SELECT group_key, performance_year,
                   any_value(participation_type) AS participation_type,
                   mode(practice_state) AS practice_state,
                   max(practice_size) AS practice_size,
                   count(DISTINCT npi) AS n_clinicians,
                   count(DISTINCT npi) FILTER (WHERE vendor_tier = 'core_ehr') AS n_core_clinicians,
                   mode(specialty) AS top_specialty,
                   count(DISTINCT npi) FILTER (WHERE npi IN (SELECT npi FROM clinician_affiliations)) AS n_affiliated
            FROM rows_ GROUP BY 1, 2
        ), per_vendor AS (
            SELECT group_key, performance_year, vendor, count(DISTINCT npi) AS n
            FROM rows_ WHERE vendor_tier = 'core_ehr' GROUP BY 1, 2, 3
        ), ranked AS (
            SELECT *, row_number() OVER (PARTITION BY group_key, performance_year ORDER BY n DESC, vendor) AS rn,
                   count(*) OVER (PARTITION BY group_key, performance_year) AS n_vendors,
                   string_agg(vendor, '|' ORDER BY n DESC, vendor) OVER (PARTITION BY group_key, performance_year) AS all_vendors
            FROM per_vendor
        ), top AS (
            SELECT r.group_key, r.performance_year, r.vendor, r.n, r.n_vendors, r.all_vendors,
                   (SELECT count(*) FROM per_vendor p WHERE p.group_key = r.group_key AND p.performance_year = r.performance_year AND p.n = r.n) > 1 AS is_tie
            FROM ranked r WHERE rn = 1
        )
        SELECT g.group_key, g.performance_year, g.participation_type, g.practice_state, g.practice_size,
               g.n_clinicians, g.n_core_clinicians,
               t.vendor,
               CASE WHEN g.n_core_clinicians > 0 THEN t.n / g.n_core_clinicians END AS vendor_share,
               coalesce(t.n_vendors, 0) AS n_vendors, t.all_vendors,
               coalesce(t.is_tie, FALSE) AS is_tie,
               g.top_specialty,
               g.n_affiliated / g.n_clinicians AS hospital_affiliated_share,
               NULL AS practice_id, NULL AS match_method, NULL AS match_share   -- filled by cms_dac::derive (#18)
        FROM grp g LEFT JOIN top t USING (group_key, performance_year);
        "#,
    )?;
    let n_pv = count(conn, "SELECT count(*) FROM practice_vendor")?;
    let n_pv_v = count(conn, "SELECT count(*) FROM practice_vendor WHERE vendor IS NOT NULL")?;
    let n_aff = count(conn, "SELECT count(*) FROM clinician_affiliations")?;
    let n_cehrt = count(conn, "SELECT count(*) FROM clinician_cehrt")?;
    let n_hcv = count(conn, "SELECT count(*) FROM hospital_clinician_vendor")?;
    let dup = count(conn, "SELECT count(*) - count(DISTINCT (ccn, performance_year, vendor)) FROM hospital_clinician_vendor")?;
    ensure!(dup == 0, "hospital_clinician_vendor has {dup} duplicate keys");
    let dup2 = count(conn, "SELECT count(*) - count(DISTINCT (npi, performance_year, chpl_id)) FROM clinician_cehrt")?;
    ensure!(dup2 == 0, "clinician_cehrt has {dup2} duplicate keys");
    let covered = count(conn, "SELECT count(DISTINCT ccn) FROM hospital_clinician_vendor WHERE rank = 1")?;
    // Views are (re)created after the derive step, so compare against hospital_vendor_year directly.
    const LATEST_VENDOR: &str = "SELECT ccn, arg_max(primary_vendor, year) AS vendor FROM hospital_vendor_year
                                 WHERE primary_vendor IS NOT NULL GROUP BY ccn";
    let agree = count(
        conn,
        &format!(
            "SELECT count(*) FROM hospital_clinician_vendor v JOIN ({LATEST_VENDOR}) h USING (ccn)
             WHERE v.rank = 1 AND v.vendor = h.vendor"
        ),
    )?;
    let comparable = count(
        conn,
        &format!(
            "SELECT count(*) FROM hospital_clinician_vendor v JOIN ({LATEST_VENDOR}) h USING (ccn) WHERE v.rank = 1"
        ),
    )?;
    info!(
        clinician_affiliations = n_aff,
        clinician_cehrt = n_cehrt,
        hospital_clinician_vendor = n_hcv,
        practice_vendor = format!("{n_pv_v}/{n_pv} with a core vendor"),
        hospitals_with_clinician_vendor = covered,
        agrees_with_hospital_vendor = format!("{agree}/{comparable}"),
        "clinician tables derived"
    );
    Ok(())
}
