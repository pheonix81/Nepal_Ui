//! AHRQ Compendium of U.S. Health Systems — Hospital Linkage File + Health Systems file.
//!
//! Two CSVs per compendium year, published at
//! `https://www.ahrq.gov/chsp/data-resources/compendium-<year>.html`:
//!
//! * `chsp-hospital-linkage-<year>.csv` — one row per hospital AHRQ could identify
//!   (~6,800 in 2023): `ccn`, name, address, `health_sys_id`/`health_sys_name`
//!   (blank ⇒ independent hospital), corporate parent (2023+), beds, discharges,
//!   residents, teaching/ownership flags, revenue.
//! * `chsp-compendium-<year>.csv` — one row per health system (~640): id, name, HQ
//!   city/state, physician and facility counts, beds, discharges, multistate flag,
//!   insurance-product flags, ownership, revenue.
//!
//! # Getting the files
//!
//! ahrq.gov shows a bot challenge to clients that do not look like a browser, so the
//! request is sent with a browser `User-Agent`; that is enough for the CSVs (verified
//! 2026-09-26). If AHRQ tightens this, download both files in a browser and register them:
//!
//! ```sh
//! ehr-atlas ahrq-compendium --year 2023 \
//!     --file ~/Downloads/chsp-hospital-linkage-2023.csv \
//!     --systems-file ~/Downloads/chsp-compendium-2023.csv
//! ```
//!
//! Use the **CSV**, not the XLS/XLSX: AHRQ's own warning is that spreadsheet tools drop the
//! leading zeros of CCNs. We read every column as text so `010001` stays `010001`.
//!
//! # Column handling
//!
//! Column names differ slightly between years (2022 has no corporate-parent columns, 2023
//! adds them and more system-level counts), so columns are resolved by lower-cased name:
//! the ones we care about become typed columns, everything else is kept in `extra` (JSON).
//!
//! # Quirks (2023 file)
//!
//! * The CSVs are Windows-1252, not UTF-8 (en dashes and curly apostrophes in a handful
//!   of names). They are transcoded to a UTF-8 copy under `data/cache/ahrq/` before loading.
//! * 124 hospitals have no CCN; they are staged with `ccn = NULL` and excluded from
//!   `hospital_systems`.
//! * A few CCNs appear on more than one row (AHRQ documents "multiple hospitals sharing
//!   the same CCN"). `hospital_systems` keeps one row per (ccn, year): the row with a
//!   system, then the larger hospital.

use crate::config::Paths;
use crate::db::{count, lit};
use crate::download::{
    all_from_manifest, check_csv_header, download_with, register_local_file, DownloadRequest,
};
use anyhow::{bail, ensure, Context, Result};
use duckdb::Connection;
use ehr_atlas_core::manifest::RawFile;
use ehr_atlas_core::sources::{
    SourceId, AHRQ_COMPENDIUM_PAGE, AHRQ_LINKAGE_CSV, AHRQ_LINKAGE_REQUIRED_COLUMNS,
    AHRQ_SYSTEMS_CSV, AHRQ_SYSTEMS_REQUIRED_COLUMNS, BROWSER_USER_AGENT,
};
use std::path::Path;
use tracing::{info, warn};

pub const SOURCE: &str = "ahrq_compendium";

fn linkage_name(year: u16) -> String {
    format!("chsp-hospital-linkage-{year}.csv")
}
fn systems_name(year: u16) -> String {
    format!("chsp-compendium-{year}.csv")
}

/// Download both files for `year` (browser UA), or register hand-supplied copies, or —
/// offline — reuse whatever the manifest already has for this source.
pub async fn fetch(
    paths: &Paths,
    client: &reqwest::Client,
    offline: bool,
    linkage_file: Option<&Path>,
    systems_file: Option<&Path>,
    year: u16,
) -> Result<Vec<RawFile>> {
    let mut files = Vec::new();
    if linkage_file.is_some() || systems_file.is_some() {
        if let Some(p) = linkage_file {
            files.push(register_local_file(
                paths,
                SOURCE,
                p,
                &linkage_name(year),
                Some(format!("AHRQ Compendium {year} hospital linkage file (hand-downloaded)")),
            )?);
        }
        if let Some(p) = systems_file {
            files.push(register_local_file(
                paths,
                SOURCE,
                p,
                &systems_name(year),
                Some(format!("AHRQ Compendium {year} health systems file (hand-downloaded)")),
            )?);
        }
        // Keep earlier years / the other file if already registered.
        if let Ok(existing) = all_from_manifest(paths, SOURCE) {
            for f in existing {
                if !files.iter().any(|x| x.id == f.id) {
                    files.push(f);
                }
            }
        }
        return Ok(files);
    }
    if offline {
        return all_from_manifest(paths, SOURCE);
    }
    let page = AHRQ_COMPENDIUM_PAGE.replace("{year}", &year.to_string());
    for (url_t, name, what) in [
        (AHRQ_LINKAGE_CSV, linkage_name(year), "hospital linkage file"),
        (AHRQ_SYSTEMS_CSV, systems_name(year), "health systems file"),
    ] {
        let url = url_t.replace("{year}", &year.to_string());
        let res = download_with(
            paths,
            client,
            DownloadRequest {
                source: SourceId::AhrqCompendium.as_str(),
                url: &url,
                file_name: &name,
                upstream_modified: None,
                notes: Some(format!("AHRQ Compendium {year} {what}; page {page}")),
            },
            |b| b.header("User-Agent", BROWSER_USER_AGENT).header("Accept", "text/csv,*/*"),
        )
        .await
        .with_context(|| {
            format!(
                "downloading the AHRQ {what} for {year}. If ahrq.gov now blocks scripted \
                 downloads, open {page} in a browser, download the CSV, and run:\n  \
                 ehr-atlas ahrq-compendium --year {year} --file <linkage.csv> --systems-file <compendium.csv>"
            )
        })?;
        // A bot-challenge page is HTML, not CSV: refuse it before it reaches the manifest's consumers.
        let path = paths.root.join(&res.path);
        let head = std::fs::read(&path).map(|b| b[..b.len().min(64)].to_ascii_lowercase())?;
        if head.starts_with(b"<!doctype") || head.starts_with(b"<html") {
            bail!(
                "{url} returned an HTML page instead of CSV (bot challenge). Download it in a \
                 browser from {page} and pass --file / --systems-file."
            );
        }
        files.push(res);
    }
    // Also carry forward earlier years already on disk so staging keeps every year.
    if let Ok(existing) = all_from_manifest(paths, SOURCE) {
        for f in existing {
            if !files.iter().any(|x| x.id == f.id) {
                files.push(f);
            }
        }
    }
    Ok(files)
}

/// Windows-1252 → UTF-8. Bytes 0x80–0x9F are the cp1252 punctuation block (en dash, curly
/// quotes, …); everything else maps like Latin-1. Valid UTF-8 input is returned unchanged.
fn cp1252_to_utf8(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    const HIGH: [char; 32] = [
        '\u{20AC}', '\u{FFFD}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
        '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{FFFD}', '\u{017D}', '\u{FFFD}',
        '\u{FFFD}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
        '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{FFFD}', '\u{017E}', '\u{0178}',
    ];
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9F => HIGH[(b - 0x80) as usize],
            _ => b as char,
        })
        .collect()
}

/// Path of a UTF-8 copy of `raw` (made under `data/cache/ahrq/` when the file is not UTF-8).
fn utf8_copy(paths: &Paths, raw: &Path) -> Result<std::path::PathBuf> {
    let bytes = std::fs::read(raw).with_context(|| format!("reading {}", raw.display()))?;
    if std::str::from_utf8(&bytes).is_ok() {
        return Ok(raw.to_path_buf());
    }
    let dir = paths.cache.join("ahrq");
    std::fs::create_dir_all(&dir)?;
    let name = raw.file_name().and_then(|s| s.to_str()).unwrap_or("ahrq.csv");
    let out = dir.join(format!("{name}.utf8.csv"));
    std::fs::write(&out, cp1252_to_utf8(&bytes))?;
    info!(from = %raw.display(), to = %out.display(), "transcoded Windows-1252 → UTF-8");
    Ok(out)
}

/// Year encoded in the stored file name (`…-<year>.csv`).
fn year_of(file: &RawFile) -> Result<u16> {
    let name = file
        .path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let stem = name.trim_end_matches(".csv");
    let tail = stem.rsplit('-').next().unwrap_or_default();
    tail.parse::<u16>()
        .with_context(|| format!("cannot read the compendium year from file name {name}"))
}

fn is_linkage(file: &RawFile) -> bool {
    file.path
        .to_string_lossy()
        .contains("chsp-hospital-linkage-")
}

/// Map lower-cased header → exact header as it appears in the file.
fn header_map(header: &[String]) -> std::collections::HashMap<String, String> {
    header
        .iter()
        .map(|h| (h.to_ascii_lowercase(), h.clone()))
        .collect()
}

/// SQL expression for a column that may be absent in this year's file.
fn col(map: &std::collections::HashMap<String, String>, name: &str) -> String {
    match map.get(name) {
        Some(h) => format!("nullif(trim(\"{}\"), '')", h.replace('"', "\"\"")),
        None => "NULL".to_string(),
    }
}

/// `to_json(struct_pack(...))` of every column not in `used`.
fn extra_json(header: &[String], used: &[&str]) -> String {
    let rest: Vec<String> = header
        .iter()
        .filter(|h| !used.iter().any(|u| u.eq_ignore_ascii_case(h)))
        .map(|h| {
            let q = h.replace('"', "\"\"");
            format!("\"{q}\" := nullif(trim(\"{q}\"), '')")
        })
        .collect();
    if rest.is_empty() {
        "NULL".into()
    } else {
        format!("to_json(struct_pack({}))", rest.join(", "))
    }
}

pub fn normalize(conn: &Connection, paths: &Paths, files: &[RawFile]) -> Result<()> {
    ensure!(!files.is_empty(), "no AHRQ Compendium files to load");
    conn.execute_batch("DELETE FROM stg_ahrq_linkage; DELETE FROM stg_ahrq_systems;")?;
    // Newest download of each (kind, year) wins.
    let mut sorted: Vec<&RawFile> = files.iter().collect();
    sorted.sort_by_key(|f| f.downloaded_at);
    let mut seen: std::collections::HashSet<(bool, u16)> = Default::default();
    for file in sorted.iter().rev() {
        let year = year_of(file)?;
        let linkage = is_linkage(file);
        if !seen.insert((linkage, year)) {
            continue;
        }
        let path = utf8_copy(paths, &paths.root.join(&file.path))?;
        let p = lit(&path.to_string_lossy());
        let fid = lit(&file.id);
        if linkage {
            let header = check_csv_header(&path, AHRQ_LINKAGE_REQUIRED_COLUMNS)?;
            let m = header_map(&header);
            let used = [
                "compendium_hospital_id", "ccn", "hospital_name", "hospital_street", "hospital_city",
                "hospital_state", "hospital_zip", "acutehosp_flag", "health_sys_id", "health_sys_name",
                "health_sys_city", "health_sys_state", "corp_parent_id", "corp_parent_name",
                "corp_parent_type", "hos_beds", "hos_dsch", "hos_res", "hos_majteach", "hos_ownership",
                "hos_net_revenue", "hos_total_revenue",
            ];
            conn.execute_batch(&format!(
                r#"
                INSERT INTO stg_ahrq_linkage
                SELECT {year} AS compendium_year,
                       {compendium_hospital_id},
                       CASE WHEN regexp_matches({ccn}, '^[0-9A-Za-z]{{1,6}}$') THEN lpad(upper({ccn}), 6, '0') END AS ccn,
                       {hospital_name}, {hospital_street}, {hospital_city}, {hospital_state}, {hospital_zip},
                       CASE {acutehosp_flag} WHEN '1' THEN TRUE WHEN '0' THEN FALSE END,
                       {health_sys_id}, {health_sys_name}, {health_sys_city}, {health_sys_state},
                       {corp_parent_id}, {corp_parent_name}, {corp_parent_type},
                       TRY_CAST({hos_beds} AS INTEGER), TRY_CAST({hos_dsch} AS INTEGER), TRY_CAST({hos_res} AS DOUBLE),
                       CASE {hos_majteach} WHEN '1' THEN TRUE WHEN '0' THEN FALSE END,
                       {hos_ownership}, TRY_CAST({hos_net_revenue} AS DOUBLE), TRY_CAST({hos_total_revenue} AS DOUBLE),
                       {extra},
                       {fid}
                FROM read_csv({p}, header=true, all_varchar=true);
                "#,
                compendium_hospital_id = col(&m, "compendium_hospital_id"),
                ccn = col(&m, "ccn"),
                hospital_name = col(&m, "hospital_name"),
                hospital_street = col(&m, "hospital_street"),
                hospital_city = col(&m, "hospital_city"),
                hospital_state = col(&m, "hospital_state"),
                hospital_zip = col(&m, "hospital_zip"),
                acutehosp_flag = col(&m, "acutehosp_flag"),
                health_sys_id = col(&m, "health_sys_id"),
                health_sys_name = col(&m, "health_sys_name"),
                health_sys_city = col(&m, "health_sys_city"),
                health_sys_state = col(&m, "health_sys_state"),
                corp_parent_id = col(&m, "corp_parent_id"),
                corp_parent_name = col(&m, "corp_parent_name"),
                corp_parent_type = col(&m, "corp_parent_type"),
                hos_beds = col(&m, "hos_beds"),
                hos_dsch = col(&m, "hos_dsch"),
                hos_res = col(&m, "hos_res"),
                hos_majteach = col(&m, "hos_majteach"),
                hos_ownership = col(&m, "hos_ownership"),
                hos_net_revenue = col(&m, "hos_net_revenue"),
                hos_total_revenue = col(&m, "hos_total_revenue"),
                extra = extra_json(&header, &used),
            ))?;
            let n = count(conn, &format!("SELECT count(*) FROM stg_ahrq_linkage WHERE compendium_year = {year}"))?;
            let no_ccn = count(conn, &format!("SELECT count(*) FROM stg_ahrq_linkage WHERE compendium_year = {year} AND ccn IS NULL"))?;
            let in_sys = count(conn, &format!("SELECT count(*) FROM stg_ahrq_linkage WHERE compendium_year = {year} AND health_sys_id IS NOT NULL"))?;
            info!(year, rows = n, without_ccn = no_ccn, in_a_system = in_sys, "stg_ahrq_linkage loaded");
        } else {
            let header = check_csv_header(&path, AHRQ_SYSTEMS_REQUIRED_COLUMNS)?;
            let m = header_map(&header);
            let used = [
                "health_sys_id", "health_sys_name", "health_sys_city", "health_sys_state", "aha_sysid",
                "total_mds", "grp_cnt", "hosp_cnt", "acutehosp_cnt", "sys_beds", "sys_dsch", "sys_multistate",
                "sys_anyins_product", "sys_ownership", "hos_net_revenue", "hos_total_revenue",
            ];
            conn.execute_batch(&format!(
                r#"
                INSERT INTO stg_ahrq_systems
                SELECT {year}, {health_sys_id}, {health_sys_name}, {health_sys_city}, {health_sys_state}, {aha_sysid},
                       TRY_CAST({total_mds} AS INTEGER), TRY_CAST({grp_cnt} AS INTEGER), TRY_CAST({hosp_cnt} AS INTEGER),
                       TRY_CAST({acutehosp_cnt} AS INTEGER), TRY_CAST({sys_beds} AS INTEGER), TRY_CAST({sys_dsch} AS INTEGER),
                       {sys_multistate},
                       CASE {sys_anyins_product} WHEN '1' THEN TRUE WHEN '0' THEN FALSE END,
                       {sys_ownership}, TRY_CAST({hos_net_revenue} AS DOUBLE), TRY_CAST({hos_total_revenue} AS DOUBLE),
                       {extra},
                       {fid}
                FROM read_csv({p}, header=true, all_varchar=true)
                WHERE {health_sys_id} IS NOT NULL;
                "#,
                health_sys_id = col(&m, "health_sys_id"),
                health_sys_name = col(&m, "health_sys_name"),
                health_sys_city = col(&m, "health_sys_city"),
                health_sys_state = col(&m, "health_sys_state"),
                aha_sysid = col(&m, "aha_sysid"),
                total_mds = col(&m, "total_mds"),
                grp_cnt = col(&m, "grp_cnt"),
                hosp_cnt = col(&m, "hosp_cnt"),
                acutehosp_cnt = col(&m, "acutehosp_cnt"),
                sys_beds = col(&m, "sys_beds"),
                sys_dsch = col(&m, "sys_dsch"),
                sys_multistate = col(&m, "sys_multistate"),
                sys_anyins_product = col(&m, "sys_anyins_product"),
                sys_ownership = col(&m, "sys_ownership"),
                hos_net_revenue = col(&m, "hos_net_revenue"),
                hos_total_revenue = col(&m, "hos_total_revenue"),
                extra = extra_json(&header, &used),
            ))?;
            let n = count(conn, &format!("SELECT count(*) FROM stg_ahrq_systems WHERE compendium_year = {year}"))?;
            info!(year, rows = n, "stg_ahrq_systems loaded");
        }
    }
    let years_l = count(conn, "SELECT count(DISTINCT compendium_year) FROM stg_ahrq_linkage")?;
    let years_s = count(conn, "SELECT count(DISTINCT compendium_year) FROM stg_ahrq_systems")?;
    if years_s < years_l {
        warn!("systems file missing for some year(s); health_systems will be incomplete");
    }
    Ok(())
}

pub fn derive(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        DELETE FROM hospital_systems;
        INSERT INTO hospital_systems
        SELECT ccn, compendium_year, health_sys_id, health_sys_name, health_sys_state,
               compendium_hospital_id, corp_parent_id, corp_parent_name, corp_parent_type,
               hos_beds, hos_discharges, hos_major_teaching, source_file_id
        FROM stg_ahrq_linkage
        WHERE ccn IS NOT NULL AND health_sys_id IS NOT NULL
        QUALIFY row_number() OVER (PARTITION BY ccn, compendium_year
                                   ORDER BY coalesce(hos_beds, 0) DESC, compendium_hospital_id) = 1;

        DELETE FROM health_systems;
        INSERT INTO health_systems
        SELECT health_sys_id, compendium_year, health_sys_name, health_sys_city, health_sys_state, aha_sysid,
               total_mds, n_groups, n_hospitals, n_acute_hospitals, n_beds, n_discharges,
               CASE WHEN multistate_code IS NULL THEN NULL ELSE multistate_code <> '1' END,
               multistate_code, any_insurance_product, ownership_code, net_revenue, total_revenue, source_file_id
        FROM stg_ahrq_systems
        QUALIFY row_number() OVER (PARTITION BY health_sys_id, compendium_year ORDER BY source_file_id) = 1;
        "#,
    )?;
    let n_hs = count(conn, "SELECT count(*) FROM hospital_systems")?;
    let n_sys = count(conn, "SELECT count(*) FROM health_systems")?;
    let dup = count(conn, "SELECT count(*) - count(DISTINCT (ccn, compendium_year)) FROM hospital_systems")?;
    ensure!(dup == 0, "hospital_systems has {dup} duplicate (ccn, compendium_year) keys");
    let orphan_sys = count(
        conn,
        "SELECT count(DISTINCT health_sys_id) FROM hospital_systems hs
         WHERE NOT EXISTS (SELECT 1 FROM health_systems s WHERE s.health_sys_id = hs.health_sys_id AND s.compendium_year = hs.compendium_year)",
    )?;
    let matched = count(
        conn,
        "SELECT count(DISTINCT ccn) FROM hospital_systems hs WHERE EXISTS (SELECT 1 FROM hospitals h WHERE h.ccn = hs.ccn)",
    )?;
    info!(
        hospital_systems = n_hs,
        health_systems = n_sys,
        system_ids_without_system_row = orphan_sys,
        ccns_matching_hospitals = matched,
        "AHRQ tables derived"
    );
    Ok(())
}
