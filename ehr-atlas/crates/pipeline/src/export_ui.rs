//! `ehr-atlas export-ui` — the compact JSON the web page embeds, plus the rendered page.
//!
//! Writes `data/processed/ui.json` and renders `web/index.html` into `web/dist/index.html`
//! by substituting the `__UI_DATA__` / `__UI_TOPO__` placeholders, then copies `web/public/*`
//! (icons) alongside it. The page embeds everything except the data it only needs once a
//! hospital or practice is pinned (`catchment.byCcn`, `zips`, `plocs`, a quarter of the
//! bytes): that goes to `web/dist/detail.<hash>.json`, named in the embedded `detail` key and
//! fetched after the map is drawn. `ui.json` keeps the full document. Everything comes from
//! `hospital_profile` (one row per CCN) so a core-only build still exports; enrichment
//! columns are simply null.
//!
//! Shape (all arrays are positional to keep the file small):
//! ```text
//! { generated, hospitals: {keys, rows}, systems: {id: [name, state, n_hospitals]},
//!   catchment: {year, max_zips, share, byCcn: {ccn: [total_cases, n_zips, [[zip, cases], …]]}},
//!   zips: {zip: [lat, lon]},
//!   practices: {keys, rows}, lpractices: {keys, rows}, plocs: {practice_id: [[lat, lon, n_clinicians], …]},
//!   history: {vendors: [name, …], byCcn: {ccn: [[first_year, last_year, vendor_index], …]},
//!             switched: {ccn: [vendor_index, last_mu_year]}},
//!   coverage: {geo: {method: n}, practices: {geo: {method: n}, groups, groups_matched}, …} }
//! ```
//! Practices (#18) are the MIPS groups matched to a Doctors & Clinicians organisation;
//! `lpractices` (#19) the organisations known only from vendors' FHIR endpoint lists (Lantern)
//! that could be placed, with a narrower column set (the page tags them `src = 'l'`); `plocs`
//! carries each practice's secondary locations (the primary one is in the row itself).
//! `history` (#29) is each hospital's attested primary vendor as runs of consecutive years,
//! `{vendors: [name, …], byCcn: {ccn: [[first_year, last_year, vendor_index], …]}}`, from
//! `hospital_vendor_history` (MU 2011–2016, PI 2023+); it only shows on the detail card, so
//! it travels in the deferred detail file too, with `history.switched`, `{ccn: [vendor_index,
//! year]}`: the hospitals whose current vendor differs from their last Meaningful Use
//! attestation (both sides decided firmly), for the page's "Switched since 2016" filter.

use crate::config::Paths;
use crate::db::count;
use anyhow::{Context, Result};
use chrono::Utc;
use duckdb::types::Value;
use duckdb::Connection;
use serde_json::{json, Map, Value as J};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tracing::info;

/// Per hospital: keep at most this many origin ZIPs …
const CATCHMENT_MAX_ZIPS: i64 = 50;
/// Per practice: secondary locations shown on the map when it is pinned.
const PRACTICE_MAX_LOCATIONS: i64 = 30;
/// … fewer for Lantern-only practices (athenahealth lists a department per ZIP; the page would double in size).
const LANTERN_MAX_LOCATIONS: i64 = 10;
/// … or stop once they cover this share of the hospital's cases, whichever comes first.
const CATCHMENT_SHARE: f64 = 0.8;

fn to_json(v: Value) -> J {
    match v {
        Value::Null => J::Null,
        Value::Boolean(b) => J::Bool(b),
        Value::TinyInt(n) => json!(n),
        Value::SmallInt(n) => json!(n),
        Value::Int(n) => json!(n),
        Value::BigInt(n) => json!(n),
        Value::HugeInt(n) => json!(n),
        Value::UTinyInt(n) => json!(n),
        Value::USmallInt(n) => json!(n),
        Value::UInt(n) => json!(n),
        Value::UBigInt(n) => json!(n),
        Value::Float(f) => json!(f),
        Value::Double(f) => json!(f),
        Value::Text(s) => J::String(s),
        Value::Date32(d) => chrono::DateTime::from_timestamp((d as i64) * 86_400, 0)
            .map(|d| J::String(d.format("%Y-%m-%d").to_string()))
            .unwrap_or(J::Null),
        other => J::String(format!("{other:?}")),
    }
}

/// Run a query and return (column names, rows as JSON arrays).
fn rows(conn: &Connection, sql: &str) -> Result<(Vec<String>, Vec<Vec<J>>)> {
    let mut stmt = conn.prepare(sql)?;
    let mut q = stmt.query([])?;
    let mut keys: Vec<String> = Vec::new();
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        if keys.is_empty() {
            keys = row.as_ref().column_names().iter().map(|s| s.to_string()).collect();
        }
        let n = row.as_ref().column_count();
        out.push((0..n).map(|i| to_json(row.get::<_, Value>(i).unwrap_or(Value::Null))).collect());
    }
    Ok((keys, out))
}

const HOSPITALS_SQL: &str = r#"
WITH fe AS (
    SELECT ccn, count(*) FILTER (WHERE http_response = 200) AS live
    FROM fhir_endpoints WHERE ccn IS NOT NULL GROUP BY ccn
)
SELECT p.ccn, p.name, p.city, p.state, p.zip,
       p.hospital_type AS type, p.ownership,
       p.vendor, p.vendor_year AS yr, p.core_vendors AS core, p.all_vendors AS "all", p.rule, p.is_tie AS tie,
       p.attested_vendor AS av, p.attested_year AS ay, strftime(p.override_as_of, '%Y-%m') AS ovAt, p.override_source AS ovSrc,
       round(p.lat, 4) AS lat, round(p.lon, 4) AS lon,
       CASE p.geo_method WHEN 'census_exact' THEN 'r' WHEN 'census_nonexact' THEN 's' WHEN 'zip_centroid' THEN 'z' END AS geo,
       p.beds, p.cost_report_fy AS bedsFy, p.overall_rating AS star,
       p.health_sys_id AS sys,
       p.clinician_vendor AS cv, round(p.clinician_vendor_share, 2) AS cvShare,
       p.n_fhir_endpoints AS fhirN, fe.live AS fhirLive, p.fhir_vendor AS fhirV, p.fhir_observed_at AS fhirAt
FROM hospital_profile p
LEFT JOIN fe USING (ccn)
ORDER BY p.state, p.name"#;

const SYSTEMS_SQL: &str = r#"
WITH hs AS (
    SELECT * FROM health_systems
    QUALIFY row_number() OVER (PARTITION BY health_sys_id ORDER BY compendium_year DESC) = 1
), used AS (
    SELECT health_sys_id, arg_max(health_sys_name, compendium_year) AS health_sys_name
    FROM hospital_systems GROUP BY 1
)
SELECT u.health_sys_id, coalesce(hs.health_sys_name, u.health_sys_name) AS name,
       hs.health_sys_state AS state, hs.n_hospitals
FROM used u LEFT JOIN hs USING (health_sys_id)"#;

/// Step-2 practices with a vendor. Column names are the keys the page reads.
const PRACTICES_SQL: &str = r#"
SELECT p.practice_id AS id, p.name, p.city, p.state, p.zip,
       p.vendor, p.vendor_year AS yr, round(p.vendor_share, 2) AS share, p.all_vendors AS "all", p.is_tie AS tie,
       round(p.lat, 4) AS lat, round(p.lon, 4) AS lon,
       CASE p.geo_method WHEN 'census_exact' THEN 'r' WHEN 'census_nonexact' THEN 's' WHEN 'zip_centroid' THEN 'z' END AS geo,
       p.n_clinicians AS clin, p.n_members AS members, p.n_mips_clinicians AS mips, p.n_groups AS groups,
       p.top_specialty AS spec, p.specialty_mix AS specs,
       p.n_locations AS nloc, p.n_states AS nstates,
       round(p.hospital_affiliated_share, 2) AS hosp, round(p.match_share, 2) AS "match",
       p.ind_pac_id IS NOT NULL AS solo,
       CASE p.vendor_source WHEN 'lantern' THEN 'l' END AS vs,
       p.lantern_vendor AS lv, p.lantern_n_endpoints AS ln, p.lantern_match_method AS lm
FROM practices p
WHERE p.vendor IS NOT NULL AND p.source = 'cms_dac'
ORDER BY p.state, p.name"#;

/// Lantern-only practices (#19), only those with coordinates: the rest have no ZIP to place
/// them and would only pad the table (they stay in `practices` and the reports). Narrower
/// columns than PRACTICES_SQL; `all` only when several lists name the organisation.
const LANTERN_PRACTICES_SQL: &str = r#"
SELECT p.practice_id AS id, p.name, p.city, p.state, left(p.zip, 5) AS zip,
       p.vendor, CASE WHEN p.n_vendors > 1 THEN p.all_vendors END AS "all",
       round(p.lat, 4) AS lat, round(p.lon, 4) AS lon,
       CASE p.geo_method WHEN 'census_exact' THEN 'r' WHEN 'census_nonexact' THEN 's' WHEN 'zip_centroid' THEN 'z' END AS geo,
       p.n_clinicians AS clin, p.n_members AS members, p.top_specialty AS spec,
       p.n_locations AS nloc, p.n_states AS nstates,
       p.lantern_n_endpoints AS ln,
       -- match method as a short code; the page expands it (solo = id prefix, tie = all_vendors present)
       CASE WHEN p.practice_id LIKE 'lantern:%' THEN NULL
            ELSE CASE p.lantern_match_method WHEN 'npi' THEN 'p' WHEN 'name_zip' THEN 'z' WHEN 'name_state' THEN 's'
                                             WHEN 'name' THEN 'n' WHEN 'fuzzy_name_state' THEN 'f' END END AS lm
FROM practices p
WHERE p.vendor IS NOT NULL AND p.source = 'lantern' AND p.lat IS NOT NULL
ORDER BY p.state, p.name"#;

/// Gaps-and-islands over `hospital_vendor_history`: consecutive attested years with the same
/// primary vendor collapse into one `(ccn, first_year, last_year, vendor)` run. A missing year
/// (including 2017–2022, which no source covers) ends a run.
const HISTORY_SQL: &str = r#"
WITH h AS (
    SELECT ccn, year, primary_vendor AS vendor,
           year - row_number() OVER (PARTITION BY ccn, primary_vendor ORDER BY year) AS island
    FROM hospital_vendor_history
    WHERE primary_vendor IS NOT NULL AND ccn IN (SELECT ccn FROM hospital_profile)
)
SELECT ccn, min(year) AS first_year, max(year) AS last_year, vendor
FROM h
GROUP BY ccn, vendor, island
ORDER BY ccn, first_year
"#;

/// Hospitals whose vendor today (`hospital_profile.vendor`, overrides included) differs from
/// their last Meaningful Use attestation (2016 for most). Both sides must be firm: the MU year
/// decided by a core product or a single vendor, today's by those or a hand-entered override;
/// module-count and tie-break picks too often reflect add-on modules rather than a switch.
const SWITCHED_SQL: &str = r#"
WITH mu AS (
    SELECT ccn, arg_max(primary_vendor, year) AS vendor, max(year) AS year, arg_max(rule, year) AS rule
    FROM hospital_vendor_year_mu WHERE primary_vendor IS NOT NULL GROUP BY ccn
)
SELECT p.ccn, mu.vendor, mu.year
FROM hospital_profile p JOIN mu USING (ccn)
WHERE p.vendor IS NOT NULL AND p.vendor <> mu.vendor
  AND mu.rule IN ('core_product', 'single_vendor')
  AND p.rule IN ('core_product', 'single_vendor', 'override')
ORDER BY p.ccn
"#;

fn practice_locations_sql() -> String {
    format!(
        r#"
SELECT l.practice_id, round(l.lat, 4) AS lat, round(l.lon, 4) AS lon, l.n_clinicians
FROM practice_locations l
JOIN practices p USING (practice_id)
WHERE p.vendor IS NOT NULL AND l.loc_rank > 1 AND l.lat IS NOT NULL
  AND l.loc_rank <= CASE p.source WHEN 'lantern' THEN {LANTERN_MAX_LOCATIONS} ELSE {PRACTICE_MAX_LOCATIONS} END
  AND (p.source <> 'lantern' OR p.lat IS NOT NULL)
ORDER BY l.practice_id, l.loc_rank"#
    )
}

fn catchment_sql() -> String {
    format!(
        r#"
WITH latest AS (SELECT max(year) AS y FROM hospital_service_area),
r AS (
    SELECT ccn, zip, total_cases,
           sum(total_cases) OVER (PARTITION BY ccn) AS tot,
           count(*) OVER (PARTITION BY ccn) AS n_zips,
           row_number() OVER (PARTITION BY ccn ORDER BY total_cases DESC, zip) AS rn,
           sum(total_cases) OVER (PARTITION BY ccn ORDER BY total_cases DESC, zip ROWS UNBOUNDED PRECEDING) AS cum
    FROM hospital_service_area
    WHERE year = (SELECT y FROM latest) AND total_cases > 0
      AND ccn IN (SELECT ccn FROM hospitals)
)
SELECT r.ccn, r.zip, r.total_cases, r.tot, r.n_zips, round(z.lat, 3) AS lat, round(z.lon, 3) AS lon,
       (SELECT y FROM latest) AS year
FROM r JOIN stg_census_zcta z USING (zip)
WHERE r.rn <= {CATCHMENT_MAX_ZIPS} AND r.cum - r.total_cases < {CATCHMENT_SHARE} * r.tot
ORDER BY r.ccn, r.rn"#
    )
}

pub fn export(conn: &Connection, paths: &Paths) -> Result<PathBuf> {
    // ---- hospitals ----
    let (keys, hrows) = rows(conn, HOSPITALS_SQL)?;
    let n_hosp = hrows.len();

    // ---- systems ----
    let (_, srows) = rows(conn, SYSTEMS_SQL)?;
    let mut systems = Map::new();
    for r in srows {
        if let J::String(id) = &r[0] {
            systems.insert(id.clone(), json!([r[1], r[2], r[3]]));
        }
    }

    // ---- catchment + the ZIP centroids it needs ----
    let (_, crows) = rows(conn, &catchment_sql())?;
    let mut by_ccn: Map<String, J> = Map::new();
    let mut zips: Map<String, J> = Map::new();
    let mut year = J::Null;
    for r in &crows {
        let (J::String(ccn), J::String(zip)) = (&r[0], &r[1]) else { continue };
        year = r[7].clone();
        zips.entry(zip.clone()).or_insert_with(|| json!([r[5], r[6]]));
        let entry = by_ccn
            .entry(ccn.clone())
            .or_insert_with(|| json!([r[3], r[4], []]));
        if let Some(list) = entry.get_mut(2).and_then(J::as_array_mut) {
            list.push(json!([zip, r[2]]));
        }
    }
    let n_catch = by_ccn.len();

    // ---- practices (#18) and their secondary locations ----
    let (pkeys, prows) = rows(conn, PRACTICES_SQL)?;
    let n_prac = prows.len();
    let (lkeys, lrows_p) = rows(conn, LANTERN_PRACTICES_SQL)?;
    let n_lprac = lrows_p.len();
    let (_, lrows) = rows(conn, &practice_locations_sql())?;
    let mut plocs: Map<String, J> = Map::new();
    for r in &lrows {
        let J::String(id) = &r[0] else { continue };
        let entry = plocs.entry(id.clone()).or_insert_with(|| json!([]));
        if let Some(list) = entry.as_array_mut() {
            list.push(json!([r[1], r[2], r[3]]));
        }
    }

    // ---- vendor history (#29): runs of consecutive years with the same primary vendor ----
    let (_, hist_rows) = rows(conn, HISTORY_SQL)?;
    let mut hist_vendors: Vec<String> = Vec::new();
    let mut hist_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut hist: Map<String, J> = Map::new();
    for r in &hist_rows {
        let (J::String(ccn), J::String(vendor)) = (&r[0], &r[3]) else { continue };
        let vi = *hist_index.entry(vendor.clone()).or_insert_with(|| {
            hist_vendors.push(vendor.clone());
            hist_vendors.len() - 1
        });
        let entry = hist.entry(ccn.clone()).or_insert_with(|| json!([]));
        if let Some(list) = entry.as_array_mut() {
            list.push(json!([r[1], r[2], vi]));
        }
    }
    let n_hist = hist.len();
    let (_, sw_rows) = rows(conn, SWITCHED_SQL)?;
    let mut switched: Map<String, J> = Map::new();
    for r in &sw_rows {
        let (J::String(ccn), J::String(vendor)) = (&r[0], &r[1]) else { continue };
        let vi = *hist_index.entry(vendor.clone()).or_insert_with(|| {
            hist_vendors.push(vendor.clone());
            hist_vendors.len() - 1
        });
        switched.insert(ccn.clone(), json!([vi, r[2]]));
    }
    let n_switched = switched.len();
    let hist_first_year = count(conn, "SELECT coalesce(min(year), 0) FROM hospital_vendor_history WHERE primary_vendor IS NOT NULL")?;

        // ---- coverage figures the page's footnote quotes ----
    let method_counts = |sql: &str| -> Result<Map<String, J>> {
        Ok(rows(conn, sql)?
            .1
            .into_iter()
            .filter_map(|r| match (&r[0], &r[1]) {
                (J::String(k), v) => Some((k.clone(), v.clone())),
                _ => None,
            })
            .collect())
    };
    let geo = method_counts("SELECT coalesce(geo_method, 'none'), count(*) FROM hospital_profile WHERE in_cms_hgi GROUP BY 1")?;
    let pgeo = method_counts("SELECT geo_method, count(*) FROM practices WHERE vendor IS NOT NULL AND source = 'cms_dac' GROUP BY 1")?;
    // Lantern (#19): how many step-2 practices the endpoint lists name, and the Lantern-only ones.
    let n_lantern_matched = count(conn, "SELECT count(*) FROM practices WHERE source = 'cms_dac' AND lantern_vendor IS NOT NULL")?;
    let n_lantern_agree = count(conn, "SELECT count(*) FROM practices WHERE source = 'cms_dac' AND vendor_source = 'mips' AND lantern_vendor = vendor")?;
    let n_lantern_filled = count(conn, "SELECT count(*) FROM practices WHERE source = 'cms_dac' AND vendor_source = 'lantern'")?;
    let n_lantern_only = count(conn, "SELECT count(*) FROM practices WHERE source = 'lantern'")?;
    let n_lantern_identified = count(conn, "SELECT count(*) FROM practices WHERE source = 'lantern' AND practice_id NOT LIKE 'lantern:%'")?;
    let n_lantern_placed = count(conn, "SELECT count(*) FROM practices WHERE source = 'lantern' AND lat IS NOT NULL")?;
    let n_groups = count(conn, "SELECT count(*) FROM practice_vendor WHERE vendor IS NOT NULL")?;
    let n_groups_matched = count(conn, "SELECT count(*) FROM practice_vendor WHERE vendor IS NOT NULL AND practice_id IS NOT NULL")?;
    let practice_year = count(conn, "SELECT coalesce(max(vendor_year), 0) FROM practices")?;
    let latest_vendor_year = count(conn, "SELECT coalesce(max(vendor_year), 0) FROM hospital_profile")?;
    let fhir_observed = rows(conn, "SELECT max(observed_at) FROM fhir_endpoints")?.1;

    let doc = json!({
        "generated": Utc::now().format("%Y-%m-%d").to_string(),
        "hospitals": { "keys": keys, "rows": hrows },
        "systems": systems,
        "catchment": {
            "year": year,
            "max_zips": CATCHMENT_MAX_ZIPS,
            "share": CATCHMENT_SHARE,
            "byCcn": by_ccn,
        },
        "zips": zips,
        "practices": { "keys": pkeys, "rows": prows },
        "lpractices": { "keys": lkeys, "rows": lrows_p },
        "plocs": plocs,
        "history": { "vendors": hist_vendors, "byCcn": hist, "switched": switched },
        "coverage": {
            "geo": geo,
            "vendor_year": latest_vendor_year,
            "history_first_year": hist_first_year,
            "fhir_observed": fhir_observed.first().map(|r| r[0].clone()).unwrap_or(J::Null),
            "practices": {
                "geo": pgeo,
                "groups": n_groups,
                "groups_matched": n_groups_matched,
                "year": practice_year,
                "max_locations": PRACTICE_MAX_LOCATIONS,
                "lantern": {
                    "matched": n_lantern_matched,
                    "agree": n_lantern_agree,
                    "filled": n_lantern_filled,
                    "only": n_lantern_only,
                    "identified": n_lantern_identified,
                    "placed": n_lantern_placed,
                },
            },
        },
    });
    let data = serde_json::to_string(&doc)?;

    std::fs::create_dir_all(&paths.processed)?;
    let json_path = paths.processed.join("ui.json");
    std::fs::write(&json_path, &data).with_context(|| format!("writing {}", json_path.display()))?;
    info!(
        path = %json_path.display(),
        hospitals = n_hosp,
        systems = systems_len(&doc),
        catchment_hospitals = n_catch,
        zips = zips_len(&doc),
        practices = n_prac,
        lantern_practices = n_lprac,
        practice_locations = lrows.len(),
        history_hospitals = n_hist,
        switched_since_mu = n_switched,
        bytes = data.len(),
        "ui.json written"
    );

    // ---- split off the detail-card-only data; the page fetches it after first paint ----
    let mut embedded = doc;
    let detail = json!({
        "catchment": embedded["catchment"].as_object_mut().and_then(|c| c.remove("byCcn")).unwrap_or(J::Null),
        "zips": embedded.as_object_mut().and_then(|d| d.remove("zips")).unwrap_or(J::Null),
        "plocs": embedded.as_object_mut().and_then(|d| d.remove("plocs")).unwrap_or(J::Null),
        "history": embedded.as_object_mut().and_then(|d| d.remove("history")).unwrap_or(J::Null),
    });
    let detail = serde_json::to_string(&detail)?;
    // Content-hashed name, so a cached page never pairs with another export's detail file.
    let detail_name = format!("detail.{}.json", &hex::encode(Sha256::digest(detail.as_bytes()))[..12]);
    embedded["detail"] = J::String(detail_name.clone());
    let data = serde_json::to_string(&embedded)?;

    // ---- render the page ----
    let template = paths.web.join("index.html");
    let topo = paths.web.join("us-states.topo.json");
    let html = std::fs::read_to_string(&template)
        .with_context(|| format!("reading {}", template.display()))?;
    let topo = std::fs::read_to_string(&topo).with_context(|| format!("reading {}", topo.display()))?;
    anyhow::ensure!(
        html.contains("__UI_DATA__") && html.contains("__UI_TOPO__"),
        "{} lacks the __UI_DATA__ / __UI_TOPO__ placeholders",
        template.display()
    );
    // `</script` inside JSON would end the embedding <script> element early.
    let safe = |s: &str| s.replace("</", "<\\/");
    let out = html
        .replacen("__UI_DATA__", &safe(&data), 1)
        .replacen("__UI_TOPO__", &safe(topo.trim()), 1);
    let dist = paths.web.join("dist");
    std::fs::create_dir_all(&dist)?;
    let page = dist.join("index.html");
    std::fs::write(&page, out).with_context(|| format!("writing {}", page.display()))?;
    // Drop earlier exports' detail files so wrangler does not keep uploading them.
    for entry in std::fs::read_dir(&dist)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.starts_with("detail.") && name.ends_with(".json") && name != detail_name {
            std::fs::remove_file(dist.join(&name))?;
        }
    }
    let detail_path = dist.join(&detail_name);
    std::fs::write(&detail_path, &detail).with_context(|| format!("writing {}", detail_path.display()))?;
    // Static files served next to the page (icons etc.).
    let public = paths.web.join("public");
    let mut n_static = 0;
    if public.is_dir() {
        for entry in std::fs::read_dir(&public)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                std::fs::copy(entry.path(), dist.join(entry.file_name()))?;
                n_static += 1;
            }
        }
    }
    info!(path = %page.display(), page_bytes = data.len(), detail = %detail_name, detail_bytes = detail.len(), static_files = n_static, "page rendered");
    Ok(page)
}

fn systems_len(doc: &J) -> usize {
    doc["systems"].as_object().map_or(0, Map::len)
}
fn zips_len(doc: &J) -> usize {
    doc["zips"].as_object().map_or(0, Map::len)
}
