//! ONC Lantern — FHIR endpoint inventory, plus the vendor-published endpoint lists it
//! indexes. A vendor signal that is days old rather than a year old.
//!
//! Two kinds of raw file, both under source `lantern`:
//!
//! 1. **Daily export** (`lantern_endpoints.csv`): the newest
//!    `lantern-daily-data/<YYYY>/<Month>/<MM_DD_YYYY>endpointdata.csv` archived in
//!    `onc-healthit/onc-open-data`. One row per (endpoint URL, list source) with the
//!    developer Lantern attributes it to, FHIR version and probe results. Its
//!    `api_information_source_name` is usually the organization name.
//! 2. **Vendor endpoint lists** (one JSON per `list_source` URL): the FHIR Bundles that
//!    Cures-Act-certified developers publish (Epic, Oracle Health, MEDITECH, Altera,
//!    MEDHOST, TruBridge, athenahealth, …). Every Endpoint resource is tied to an
//!    Organization — contained, referenced, or referencing the endpoint — which gives
//!    names and often an address or NPI. Only lists of developers whose canonical vendor is
//!    `core_ehr`-tier are downloaded (ambulatory-only vendors never match hospitals).
//!
//! `normalize` loads the CSV into `stg_lantern_endpoints` and every Bundle into
//! `stg_lantern_orgs` (endpoint URL × organization name). `derive` maps developers to
//! canonical vendors and matches organization names to CCNs by NPI, exact normalised
//! name, then Jaro-Winkler within the same state.
//!
//! # Practices (#19)
//!
//! Organizations of core-EHR vendors that are not hospitals (~300k rows, mostly
//! athenahealth, eClinicalWorks, Office Ally, Practice Fusion, AdvancedMD, NextGen) are
//! collapsed to one *organization* per NPI when the list carries one (athenahealth lists a
//! row per department) or per (name, state, ZIP) otherwise, then matched to the CMS Doctors
//! & Clinicians organisations (`stg_cms_dac`, which the step-2 practices are a subset of):
//!
//! 1. `npi` — the NPI is a clinician in D&C → their organisation (in the list's state when
//!    several; a known practice first), else their solo enrollment;
//! 2. `name_zip` — same normalised name at the same ZIP;
//! 3. `name_state` — same name, unique in the state;
//! 4. `name` — no state on the Lantern side; the name is unique nationally (≥ 8 chars, ≥ 2 words);
//! 5. `fuzzy_name_state` — Jaro-Winkler ≥ [`PRACTICE_FUZZY_MIN`] against step-2 practices in the state.
//!
//! A match to an existing practice fills `practices.lantern_*` and `fhir_endpoints.practice_id`;
//! MIPS stays the vendor of record and the Lantern developer is "also listed by" (it fills
//! `vendor` only when MIPS gave none, `vendor_source = 'lantern'`). Every other organization
//! becomes a `practices` row with `source = 'lantern'`: identified in D&C (`org:` / `ind:` id,
//! name, clinicians, specialty and addresses from D&C) or not (`lantern:<hash>`, the list's
//! name and addresses). Locations use ZIP centroids (or the geocode cache when the address is
//! already there); organizations without any ZIP get no coordinates.

use crate::config::Paths;
use crate::db::{count, lit};
use crate::download::{
    all_from_manifest, check_csv_header, download, download_with, github_dir, DownloadRequest,
};
use anyhow::{Context, Result};
use chrono::{Datelike, NaiveDate, Utc};
use duckdb::Connection;
use ehr_atlas_core::manifest::RawFile;
use ehr_atlas_core::sources::{
    SourceId, LANTERN_DAILY_DIR, ONC_OPEN_DATA_MEDIA, ONC_OPEN_DATA_REPO,
};
use ehr_atlas_core::vendor::{VendorMap, VendorTier};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::BufReader;
use std::path::Path;
use tracing::{info, warn};

pub const SOURCE: &str = "lantern";
/// Jaro-Winkler floor for the fuzzy organization → practice match (within a state).
pub const PRACTICE_FUZZY_MIN: f64 = 0.95;
const DAILY_FILE_NAME: &str = "lantern_endpoints.csv";
/// Vendor lists bigger than this are skipped (athenahealth's is ~130 MB and almost
/// entirely ambulatory practices; raise the cap if that changes).
const MAX_BUNDLE_BYTES: u64 = 256 * 1024 * 1024;

pub const LANTERN_REQUIRED_COLUMNS: &[&str] = &[
    "url",
    "api_information_source_name",
    "list_source",
    "api_developer_name",
    "capability_fhir_version",
    "http_response",
    "updated",
];

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn is_daily_export(f: &RawFile) -> bool {
    f.path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(DAILY_FILE_NAME))
}

/// Newest daily export in the archive: walk back month by month (up to a year).
async fn newest_daily_export(client: &reqwest::Client) -> Result<(String, NaiveDate)> {
    let today = Utc::now().date_naive();
    let (mut y, mut m) = (today.year(), today.month());
    for _ in 0..12 {
        let dir = format!("{LANTERN_DAILY_DIR}/{y}/{}", MONTHS[(m - 1) as usize]);
        let entries = match github_dir(client, ONC_OPEN_DATA_REPO, &dir).await {
            Ok(e) => e,
            Err(e) => {
                warn!(dir, error = %e, "no Lantern archive directory; trying previous month");
                Vec::new()
            }
        };
        let mut best: Option<(NaiveDate, String)> = None;
        for e in entries.into_iter().filter(|e| !e.is_dir) {
            if let Some(stem) = e.name.strip_suffix("endpointdata.csv") {
                if let Ok(d) = NaiveDate::parse_from_str(stem, "%m_%d_%Y") {
                    if best.as_ref().is_none_or(|(bd, _)| d > *bd) {
                        best = Some((d, e.path.clone()));
                    }
                }
            }
        }
        if let Some((d, path)) = best {
            return Ok((path, d));
        }
        if m == 1 {
            m = 12;
            y -= 1;
        } else {
            m -= 1;
        }
    }
    anyhow::bail!("no Lantern daily export found in the last 12 months of {ONC_OPEN_DATA_REPO}")
}

/// Distinct (developer, list_source) pairs in the daily export.
fn list_sources(path: &Path) -> Result<Vec<(String, String)>> {
    let mut rdr = csv::ReaderBuilder::new().flexible(true).from_path(path)?;
    let headers = rdr.headers()?.clone();
    let idx = |name: &str| headers.iter().position(|h| h == name);
    let (Some(i_dev), Some(i_ls)) = (idx("api_developer_name"), idx("list_source")) else {
        anyhow::bail!("{}: missing api_developer_name / list_source columns", path.display());
    };
    let mut seen = BTreeSet::new();
    for rec in rdr.records() {
        let rec = rec?;
        let dev = rec.get(i_dev).unwrap_or("").trim();
        let ls = rec.get(i_ls).unwrap_or("").trim();
        if !dev.is_empty() && ls.starts_with("http") {
            seen.insert((dev.to_string(), ls.to_string()));
        }
    }
    Ok(seen.into_iter().collect())
}

fn bundle_file_name(url: &str) -> String {
    let stripped = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let mut s: String = stripped
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' })
        .collect();
    s.truncate(80);
    format!("list_{}.json", s.trim_matches('_'))
}

pub async fn fetch(paths: &Paths, client: &reqwest::Client, offline: bool) -> Result<Vec<RawFile>> {
    if offline {
        return all_from_manifest(paths, SOURCE);
    }
    let (path, date) = newest_daily_export(client).await?;
    let raw_url = format!("https://raw.githubusercontent.com/{ONC_OPEN_DATA_REPO}/main/{path}");
    let mut daily = download(
        paths,
        client,
        DownloadRequest {
            source: SourceId::Lantern.as_str(),
            url: &raw_url,
            file_name: DAILY_FILE_NAME,
            upstream_modified: Some(date.to_string()),
            notes: Some(format!("{ONC_OPEN_DATA_REPO}/{path}")),
        },
    )
    .await?;
    if daily.bytes < 1024 {
        // Git LFS pointer instead of content: go through the media host.
        let media_url = format!("{ONC_OPEN_DATA_MEDIA}{path}");
        warn!(bytes = daily.bytes, "raw URL returned an LFS pointer; refetching via media host");
        daily = download(
            paths,
            client,
            DownloadRequest {
                source: SourceId::Lantern.as_str(),
                url: &media_url,
                file_name: DAILY_FILE_NAME,
                upstream_modified: Some(date.to_string()),
                notes: Some(format!("{ONC_OPEN_DATA_REPO}/{path}")),
            },
        )
        .await?;
    }
    let mut files = vec![daily.clone()];

    // Vendor-published endpoint lists for core-EHR developers.
    let vmap = VendorMap::load(&paths.config.join("vendor_map.csv"))?;
    let pairs = list_sources(&paths.root.join(&daily.path))?;
    let mut wanted: Vec<(String, String)> = Vec::new();
    let mut seen_urls = HashSet::new();
    for (dev, ls) in pairs {
        let core = vmap
            .lookup(&dev)
            .is_some_and(|r| r.vendor_tier == VendorTier::CoreEhr);
        if core && seen_urls.insert(ls.clone()) {
            wanted.push((dev, ls));
        }
    }
    info!(lists = wanted.len(), "vendor endpoint lists to fetch (core-EHR developers)");
    for (dev, ls) in wanted {
        // Size gate via HEAD when the host answers it; otherwise just try.
        if let Ok(h) = client.head(&ls).send().await {
            if let Some(len) = h.content_length() {
                if len > MAX_BUNDLE_BYTES {
                    warn!(developer = dev, url = ls, bytes = len, "vendor list too large; skipped");
                    continue;
                }
            }
        }
        let file_name = bundle_file_name(&ls);
        let res = download_with(
            paths,
            client,
            DownloadRequest {
                source: SourceId::Lantern.as_str(),
                url: &ls,
                file_name: &file_name,
                upstream_modified: None,
                notes: Some(format!("list_source for {dev}")),
            },
            |b| b.header("Accept", "application/fhir+json, application/json"),
        )
        .await;
        match res {
            Ok(f) => {
                if f.bytes > MAX_BUNDLE_BYTES {
                    warn!(developer = dev, url = ls, bytes = f.bytes, "vendor list too large; will be skipped at load");
                }
                files.push(f)
            }
            Err(e) => warn!(developer = dev, url = ls, error = %e, "vendor list download failed; skipped"),
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    Ok(files)
}

// ---------------------------------------------------------------------------
// Bundle parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
struct Org {
    name: Option<String>,
    npi: Option<String>,
    address: Option<String>,
    city: Option<String>,
    state: Option<String>,
    zip: Option<String>,
    endpoint_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OrgRow {
    pub url: String,
    pub org_name: String,
    pub npi: Option<String>,
    pub address: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub zip: Option<String>,
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_string)
}

fn parse_org(v: &Value) -> Org {
    let mut o = Org {
        name: s(v, "name"),
        ..Default::default()
    };
    if let Some(ids) = v.get("identifier").and_then(Value::as_array) {
        for id in ids {
            let sys = s(id, "system").unwrap_or_default().to_ascii_lowercase();
            let typ = id
                .get("type")
                .and_then(|t| t.get("coding"))
                .and_then(Value::as_array)
                .and_then(|c| c.first())
                .and_then(|c| s(c, "code"))
                .unwrap_or_default();
            if sys.contains("us-npi") || typ == "NPI" {
                if let Some(val) = s(id, "value") {
                    if val.len() == 10 && val.chars().all(|c| c.is_ascii_digit()) {
                        o.npi = Some(val);
                    }
                }
            }
        }
    }
    if let Some(addr) = v.get("address").and_then(Value::as_array).and_then(|a| a.first()) {
        o.address = addr
            .get("line")
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|x| !x.trim().is_empty())
            .or_else(|| s(addr, "text"));
        o.city = s(addr, "city");
        o.state = s(addr, "state");
        o.zip = s(addr, "postalCode").map(|z| z.chars().take(5).collect());
    }
    if let Some(eps) = v.get("endpoint").and_then(Value::as_array) {
        for ep in eps {
            if let Some(r) = s(ep, "reference") {
                o.endpoint_refs.push(r);
            }
        }
    }
    o
}

/// Endpoint.name is a fallback only; strip the boilerplate vendors wrap around it.
fn clean_endpoint_name(n: &str) -> Option<String> {
    let mut t = n.trim().to_string();
    for p in ["FHIR R4 Endpoint for", "Bulk FHIR R4 Endpoint for", "FHIR Endpoint for"] {
        if let Some(rest) = t.strip_prefix(p) {
            t = rest.trim().to_string();
        }
    }
    for sfx in [" - FHIR R4 Base URL", " FHIR R4 Base URL", " - FHIR R4", " endpoint", " Endpoint"] {
        if let Some(rest) = t.strip_suffix(sfx) {
            t = rest.trim().to_string();
        }
    }
    let lower = t.to_ascii_lowercase();
    if t.len() < 3 || lower.contains("base endpoint") || lower == "fhir" || lower.starts_with("nextgen-base") {
        return None;
    }
    Some(t)
}

fn ref_keys(id: Option<&str>, full_url: Option<&str>, rtype: &str) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(id) = id {
        keys.push(id.to_string());
        keys.push(format!("#{id}"));
        keys.push(format!("{rtype}/{id}"));
        keys.push(format!("urn:uuid:{id}"));
    }
    if let Some(u) = full_url {
        keys.push(u.to_string());
    }
    keys
}

/// Extract (endpoint URL, organization) rows from one vendor Bundle.
pub fn parse_bundle(bundle: &Value) -> Vec<OrgRow> {
    let entries: Vec<&Value> = match bundle.get("entry").and_then(Value::as_array) {
        Some(a) => a.iter().collect(),
        None => bundle.as_array().map(|a| a.iter().collect()).unwrap_or_default(),
    };
    // Pass 1: index endpoints (→ url) and organizations (→ Org) by every way they can be referenced.
    let mut ep_url: HashMap<String, String> = HashMap::new();
    let mut orgs: HashMap<String, Org> = HashMap::new();
    let mut endpoints: Vec<(&Value, Option<String>)> = Vec::new();
    let mut org_list: Vec<Org> = Vec::new();
    for e in &entries {
        let full = s(e, "fullUrl");
        let r = e.get("resource").unwrap_or(e);
        match r.get("resourceType").and_then(Value::as_str) {
            Some("Endpoint") => {
                if let Some(url) = s(r, "address") {
                    for k in ref_keys(s(r, "id").as_deref(), full.as_deref(), "Endpoint") {
                        ep_url.insert(k, url.clone());
                    }
                }
                endpoints.push((r, full));
            }
            Some("Organization") => {
                let o = parse_org(r);
                for k in ref_keys(s(r, "id").as_deref(), full.as_deref(), "Organization") {
                    orgs.insert(k, o.clone());
                }
                org_list.push(o);
            }
            _ => {}
        }
    }
    let mut out: Vec<OrgRow> = Vec::new();
    let mut push = |url: &str, o: &Org, name_override: Option<String>| {
        let Some(name) = name_override.or_else(|| o.name.clone()) else { return };
        let name = name.trim().trim_start_matches('*').trim().to_string();
        if name.is_empty() {
            return;
        }
        out.push(OrgRow {
            url: url.trim().to_string(),
            org_name: name,
            npi: o.npi.clone(),
            address: o.address.clone(),
            city: o.city.clone(),
            state: o.state.clone(),
            zip: o.zip.clone(),
        });
    };
    // Pass 2a: endpoints → their organizations.
    for (r, _full) in &endpoints {
        let Some(url) = s(r, "address") else { continue };
        let mut found = false;
        if let Some(contained) = r.get("contained").and_then(Value::as_array) {
            for c in contained {
                if c.get("resourceType").and_then(Value::as_str) == Some("Organization") {
                    let o = parse_org(c);
                    if o.name.is_some() {
                        push(&url, &o, None);
                        found = true;
                    }
                }
            }
        }
        if let Some(mo) = r.get("managingOrganization") {
            if let Some(reference) = s(mo, "reference") {
                if let Some(o) = orgs.get(&reference).or_else(|| orgs.get(reference.trim_start_matches('#'))) {
                    if o.name.is_some() {
                        push(&url, o, None);
                        found = true;
                    }
                }
            }
            if !found {
                if let Some(d) = s(mo, "display") {
                    push(&url, &Org::default(), Some(d));
                    found = true;
                }
            }
        }
        if !found {
            if let Some(n) = s(r, "name").and_then(|n| clean_endpoint_name(&n)) {
                push(&url, &Org::default(), Some(n));
            }
        }
    }
    // Pass 2b: organizations → their endpoints.
    for o in &org_list {
        if o.name.is_none() {
            continue;
        }
        for reference in &o.endpoint_refs {
            if let Some(url) = ep_url.get(reference).or_else(|| ep_url.get(reference.trim_start_matches('#'))) {
                push(url, o, None);
            }
        }
    }
    let mut seen = HashSet::new();
    out.retain(|r| seen.insert((r.url.to_ascii_lowercase(), r.org_name.to_ascii_uppercase())));
    out
}

// ---------------------------------------------------------------------------
// normalize / derive
// ---------------------------------------------------------------------------

pub fn normalize(conn: &Connection, paths: &Paths, files: &[RawFile]) -> Result<()> {
    let daily = files
        .iter()
        .filter(|f| is_daily_export(f))
        .max_by_key(|f| f.downloaded_at)
        .context("no Lantern daily export among the files; run `ehr-atlas lantern`")?;
    let path = paths.root.join(&daily.path);
    check_csv_header(&path, LANTERN_REQUIRED_COLUMNS)?;
    conn.execute_batch(&format!(
        r#"
        DELETE FROM stg_lantern_endpoints;
        INSERT INTO stg_lantern_endpoints
        SELECT
            trim(url)                                                   AS url,
            nullif(trim(list_source), '')                               AS list_source,
            nullif(trim(api_developer_name), '')                        AS api_developer_name,
            nullif(trim(capability_fhir_version), '')                   AS capability_fhir_version,
            nullif(trim(format), '')                                    AS format,
            TRY_CAST(nullif(trim(http_response), '') AS INTEGER)        AS http_response,
            TRY_CAST(nullif(trim(smart_http_response), '') AS INTEGER)  AS smart_http_response,
            nullif(trim(kind), '')                                      AS kind,
            CASE WHEN lower(trim(cap_stat_exists)) LIKE 'true%' THEN TRUE
                 WHEN lower(trim(cap_stat_exists)) LIKE 'false%' THEN FALSE END AS cap_stat_exists,
            nullif(trim(source), '')                                    AS source,
            TRY_CAST(strptime(nullif(trim(created_at), ''), '%m/%d/%y %H:%M') AS TIMESTAMP) AS created_at,
            TRY_CAST(strptime(nullif(trim(updated), ''), '%m/%d/%y %H:%M') AS TIMESTAMP)    AS updated_at,
            {fid} AS source_file_id
        FROM read_csv({p}, header=true, all_varchar=true)
        WHERE trim(url) <> '';
        "#,
        p = lit(&path.to_string_lossy()),
        fid = lit(&daily.id),
    ))?;
    let n_ep = count(conn, "SELECT count(*) FROM stg_lantern_endpoints")?;

    // Vendor lists: newest file per URL.
    let mut newest: HashMap<&str, &RawFile> = HashMap::new();
    for f in files.iter().filter(|f| !is_daily_export(f)) {
        let e = newest.entry(f.url.as_str()).or_insert(f);
        if f.downloaded_at > e.downloaded_at {
            *e = f;
        }
    }
    let cache_dir = paths.cache.join("lantern");
    std::fs::create_dir_all(&cache_dir)?;
    let orgs_csv = cache_dir.join("orgs.csv");
    let mut w = csv::Writer::from_path(&orgs_csv)?;
    w.write_record(["url", "org_name", "npi", "address", "city", "state", "zip", "list_source", "source_file_id"])?;
    let mut n_rows = 0usize;
    let mut n_bundles = 0usize;
    let mut bundle_files: Vec<&RawFile> = newest.into_values().collect();
    bundle_files.sort_by(|a, b| a.url.cmp(&b.url));
    for f in bundle_files {
        if f.bytes > MAX_BUNDLE_BYTES {
            warn!(url = f.url, bytes = f.bytes, "vendor list too large; skipped");
            continue;
        }
        let p = paths.root.join(&f.path);
        let file = std::fs::File::open(&p)?;
        let v: Value = match serde_json::from_reader(BufReader::new(file)) {
            Ok(v) => v,
            Err(e) => {
                warn!(url = f.url, error = %e, "vendor list is not JSON; skipped");
                continue;
            }
        };
        let rows = parse_bundle(&v);
        if rows.is_empty() {
            warn!(url = f.url, "vendor list yielded no (endpoint, organization) rows");
        }
        n_bundles += 1;
        for r in rows {
            w.write_record([
                r.url.as_str(),
                r.org_name.as_str(),
                r.npi.as_deref().unwrap_or(""),
                r.address.as_deref().unwrap_or(""),
                r.city.as_deref().unwrap_or(""),
                r.state.as_deref().unwrap_or(""),
                r.zip.as_deref().unwrap_or(""),
                f.url.as_str(),
                f.id.as_str(),
            ])?;
            n_rows += 1;
        }
    }
    w.flush()?;
    drop(w);
    conn.execute_batch(&format!(
        r#"
        DELETE FROM stg_lantern_orgs;
        INSERT INTO stg_lantern_orgs
        SELECT DISTINCT ON (lower(rtrim(url, '/')), upper(org_name))
               url, org_name, nullif(npi, ''), nullif(address, ''), nullif(city, ''),
               nullif(state, ''), nullif(zip, ''), list_source, source_file_id
        FROM read_csv({p}, header=true, all_varchar=true);
        -- Organization names Lantern itself carries, for endpoints the vendor lists did not name.
        INSERT INTO stg_lantern_orgs
        SELECT DISTINCT ON (lower(rtrim(e.url, '/')), upper(trim(e.api_information_source_name)))
               e.url, trim(e.api_information_source_name), NULL, NULL, NULL, NULL, NULL, e.list_source, {fid}
        FROM read_csv({p_daily}, header=true, all_varchar=true) e
        WHERE trim(e.api_information_source_name) <> ''
          AND lower(trim(e.api_information_source_name)) NOT IN
              ('test', 'client', 'auto testing account', 'main office', 'demo', 'sandbox', 'unknown', 'n/a', 'na', 'none')
          AND NOT EXISTS (SELECT 1 FROM stg_lantern_orgs o
                          WHERE lower(rtrim(o.url, '/')) = lower(rtrim(e.url, '/'))
                            AND upper(o.org_name) = upper(trim(e.api_information_source_name)));
        "#,
        p = lit(&orgs_csv.to_string_lossy()),
        p_daily = lit(&path.to_string_lossy()),
        fid = lit(&daily.id),
    ))?;
    let n_org = count(conn, "SELECT count(*) FROM stg_lantern_orgs")?;
    info!(
        endpoints = n_ep,
        vendor_lists = n_bundles,
        list_org_rows = n_rows,
        org_rows = n_org,
        "stg_lantern_endpoints / stg_lantern_orgs loaded"
    );
    Ok(())
}

/// `fhir_endpoints`: one row per (endpoint URL, organization name), vendor canonicalised,
/// matched to a CCN where the organization is a hospital we know; then the practice pass
/// (#19, see the module docs): non-hospital organizations matched to practices or added as
/// Lantern-only practices.
pub fn derive(conn: &Connection) -> Result<()> {
    conn.execute_batch(DERIVE_SQL)?;
    let n = count(conn, "SELECT count(*) FROM fhir_endpoints")?;
    let matched = count(conn, "SELECT count(DISTINCT ccn) FROM fhir_endpoints WHERE ccn IS NOT NULL")?;
    let dup = count(
        conn,
        "SELECT count(*) FROM (SELECT endpoint_url, org_name, count(*) c FROM fhir_endpoints GROUP BY 1,2 HAVING c > 1)",
    )?;
    anyhow::ensure!(dup == 0, "fhir_endpoints has {dup} duplicate (endpoint_url, org_name) keys");
    info!(rows = n, matched_ccns = matched, "fhir_endpoints derived");

    conn.execute_batch(&practice_sql())?;
    let orphan = count(
        conn,
        "SELECT count(*) FROM fhir_endpoints f WHERE practice_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM practices p WHERE p.practice_id = f.practice_id)",
    )?;
    anyhow::ensure!(orphan == 0, "{orphan} fhir_endpoints rows point at a practice_id missing from practices");
    let dup = count(conn, "SELECT count(*) - count(DISTINCT (practice_id, address_id)) FROM practice_locations")?;
    anyhow::ensure!(dup == 0, "practice_locations has {dup} duplicate keys");
    let by_method: Vec<String> = conn
        .prepare(
            "SELECT coalesce(practice_match_method, 'lantern_only') || ':' || count(*) FROM fhir_endpoints
             WHERE practice_id IS NOT NULL GROUP BY practice_match_method ORDER BY count(*) DESC",
        )?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    let matched_practices = count(conn, "SELECT count(*) FROM practices WHERE source = 'cms_dac' AND lantern_vendor IS NOT NULL")?;
    let filled = count(conn, "SELECT count(*) FROM practices WHERE source = 'cms_dac' AND vendor_source = 'lantern'")?;
    let lantern_only = count(conn, "SELECT count(*) FROM practices WHERE source = 'lantern'")?;
    let identified = count(conn, "SELECT count(*) FROM practices WHERE source = 'lantern' AND practice_id NOT LIKE 'lantern:%'")?;
    let placed = count(conn, "SELECT count(*) FROM practices WHERE source = 'lantern' AND lat IS NOT NULL")?;
    info!(
        endpoints_by_match = ?by_method,
        practices_matched = matched_practices,
        practices_vendor_filled = filled,
        lantern_only_practices = lantern_only,
        lantern_only_identified_in_dac = identified,
        lantern_only_placed = placed,
        "Lantern organizations → practices derived"
    );
    Ok(())
}

/// The practice pass of [`derive`]; reads `_core` / `_match` left by [`DERIVE_SQL`].
fn practice_sql() -> String {
    let new_ids = "(SELECT practice_id, org_pac_id, ind_pac_id FROM _nids WHERE org_pac_id IS NOT NULL OR ind_pac_id IS NOT NULL)";
    format!(
        r#"
-- ===== Practices (#19): core-EHR vendors' organizations that are not hospitals =====

-- Forget the previous run: Lantern-only practices and what Lantern added to the others.
DELETE FROM practices WHERE source = 'lantern';
DELETE FROM practice_locations WHERE practice_id NOT IN (SELECT practice_id FROM practices);
UPDATE practices
SET vendor = CASE WHEN vendor_source = 'lantern' THEN NULL ELSE vendor END,
    n_vendors = CASE WHEN vendor_source = 'lantern' THEN 0 ELSE n_vendors END,
    all_vendors = CASE WHEN vendor_source = 'lantern' THEN NULL ELSE all_vendors END,
    is_tie = CASE WHEN vendor_source = 'lantern' THEN FALSE ELSE is_tie END,
    vendor_source = CASE WHEN vendor_source = 'lantern' THEN NULL ELSE vendor_source END,
    lantern_vendor = NULL, lantern_n_endpoints = NULL, lantern_match_method = NULL, lantern_match_score = NULL;

-- Organization rows of core-EHR vendors not matched to a hospital.
CREATE OR REPLACE TEMP TABLE _lo AS
SELECT c.rid, c.endpoint_url, c.org_name, c.vendor, c.observed_at, c.source_file_id, c.npi, c.address, c.city, c.state,
       CASE WHEN regexp_matches(coalesce(c.zip, ''), '^[0-9]{{5}}$') THEN c.zip END AS zip5,
       c.norm,
       coalesce('npi:' || c.npi,
                'name:' || c.norm || '|' || coalesce(c.state, '') || '|'
                        || CASE WHEN regexp_matches(coalesce(c.zip, ''), '^[0-9]{{5}}$') THEN c.zip ELSE '' END) AS gkey
FROM _core c
WHERE c.rid NOT IN (SELECT rid FROM _match) AND length(c.norm) >= 3;

-- One organization per grain, named by its most frequent name (shortest on ties).
CREATE OR REPLACE TEMP TABLE _grain AS
WITH nm AS (
    SELECT gkey, org_name, norm,
           row_number() OVER (PARTITION BY gkey ORDER BY count(*) DESC, length(org_name), org_name) AS rn
    FROM _lo GROUP BY 1, 2, 3
)
SELECT l.gkey, any_value(l.npi) AS npi, any_value(nm.org_name) AS org_name, any_value(nm.norm) AS norm,
       mode(l.state) AS state, mode(l.zip5) AS zip5,
       count(*) AS n_endpoints, max(l.observed_at) AS observed_at, any_value(l.source_file_id) AS source_file_id
FROM _lo l JOIN nm ON nm.gkey = l.gkey AND nm.rn = 1
GROUP BY 1;

-- Doctors & Clinicians organisations (empty when cms-dac has not run; every match below then fails harmlessly).
CREATE OR REPLACE TEMP TABLE _dorg AS
SELECT org_pac_id, _squash(any_value(org_name)) AS norm, count(DISTINCT npi) AS n_clinicians
FROM stg_cms_dac WHERE org_pac_id IS NOT NULL GROUP BY 1;
CREATE OR REPLACE TEMP TABLE _dname AS SELECT norm, count(*) AS n_org FROM _dorg GROUP BY 1;
CREATE OR REPLACE TEMP TABLE _dloc AS
SELECT DISTINCT org_pac_id, state, left(zip, 5) AS zip5 FROM stg_cms_dac WHERE org_pac_id IS NOT NULL AND state IS NOT NULL;
CREATE OR REPLACE TEMP TABLE _dnpi AS
SELECT d.npi, d.org_pac_id, min(d.ind_pac_id) AS ind_pac_id, list(DISTINCT d.state) AS states,
       coalesce(any_value(o.n_clinicians), 1) AS n_clinicians
FROM stg_cms_dac d LEFT JOIN _dorg o USING (org_pac_id) GROUP BY 1, 2;
CREATE OR REPLACE TEMP TABLE _known AS
SELECT practice_id, org_pac_id, ind_pac_id FROM practices WHERE source = 'cms_dac';

-- (1) NPI: a clinician in D&C → their organisation (in the list's state when several, a known practice first, the largest), else their solo enrollment.
CREATE OR REPLACE TEMP TABLE _m1 AS
SELECT gkey, practice_id, org_pac_id, ind_pac_id, 'npi' AS method, CAST(CASE WHEN n_cand = 1 THEN 1.0 ELSE 0.9 END AS DOUBLE) AS score
FROM (
    SELECT g.gkey,
           CASE WHEN d.org_pac_id IS NOT NULL THEN 'org:' || d.org_pac_id ELSE 'ind:' || d.ind_pac_id END AS practice_id,
           d.org_pac_id, CASE WHEN d.org_pac_id IS NULL THEN d.ind_pac_id END AS ind_pac_id,
           count(*) OVER (PARTITION BY g.gkey) AS n_cand,
           row_number() OVER (PARTITION BY g.gkey
                              ORDER BY (d.org_pac_id IS NOT NULL) DESC, coalesce(list_contains(d.states, g.state), FALSE) DESC,
                                       (k.practice_id IS NOT NULL) DESC, d.n_clinicians DESC, d.org_pac_id, d.ind_pac_id) AS rn
    FROM _grain g
    JOIN _dnpi d ON d.npi = g.npi
    LEFT JOIN _known k ON k.practice_id = CASE WHEN d.org_pac_id IS NOT NULL THEN 'org:' || d.org_pac_id ELSE 'ind:' || d.ind_pac_id END
    WHERE g.npi IS NOT NULL AND (d.org_pac_id IS NOT NULL OR d.ind_pac_id IS NOT NULL)
) WHERE rn = 1;

-- (2) same normalised name at the same ZIP (the largest organisation when several share both).
CREATE OR REPLACE TEMP TABLE _m2 AS
SELECT gkey, 'org:' || org_pac_id AS practice_id, org_pac_id, CAST(NULL AS TEXT) AS ind_pac_id, 'name_zip' AS method,
       CAST(CASE WHEN n_cand = 1 THEN 1.0 ELSE 0.95 END AS DOUBLE) AS score
FROM (
    SELECT g.gkey, o.org_pac_id, count(*) OVER (PARTITION BY g.gkey) AS n_cand,
           row_number() OVER (PARTITION BY g.gkey ORDER BY (k.practice_id IS NOT NULL) DESC, o.n_clinicians DESC, o.org_pac_id) AS rn
    FROM _grain g
    JOIN _dorg o ON o.norm = g.norm
    JOIN (SELECT DISTINCT org_pac_id, zip5 FROM _dloc) l ON l.org_pac_id = o.org_pac_id AND l.zip5 = g.zip5
    LEFT JOIN _known k ON k.org_pac_id = o.org_pac_id
    WHERE g.zip5 IS NOT NULL AND length(g.norm) >= 4 AND g.gkey NOT IN (SELECT gkey FROM _m1)
) WHERE rn = 1;

-- (3) same name, the only organisation of that name in the state.
CREATE OR REPLACE TEMP TABLE _m3 AS
SELECT g.gkey, 'org:' || any_value(o.org_pac_id) AS practice_id, any_value(o.org_pac_id) AS org_pac_id, CAST(NULL AS TEXT) AS ind_pac_id,
       'name_state' AS method, CAST(0.95 AS DOUBLE) AS score
FROM _grain g
JOIN _dorg o ON o.norm = g.norm
JOIN (SELECT DISTINCT org_pac_id, state FROM _dloc) l ON l.org_pac_id = o.org_pac_id AND l.state = g.state
WHERE g.state IS NOT NULL AND length(g.norm) >= 6 AND g.norm LIKE '% %'
  AND g.gkey NOT IN (SELECT gkey FROM _m1) AND g.gkey NOT IN (SELECT gkey FROM _m2)
GROUP BY g.gkey HAVING count(DISTINCT o.org_pac_id) = 1;

-- (4) no state on the Lantern side: the name is unique nationally and specific enough.
CREATE OR REPLACE TEMP TABLE _m4 AS
SELECT g.gkey, 'org:' || o.org_pac_id AS practice_id, o.org_pac_id, CAST(NULL AS TEXT) AS ind_pac_id, 'name' AS method, CAST(0.9 AS DOUBLE) AS score
FROM _grain g
JOIN _dname n ON n.norm = g.norm AND n.n_org = 1
JOIN _dorg o ON o.norm = g.norm
WHERE g.state IS NULL AND length(g.norm) >= 8 AND g.norm LIKE '% %'
  AND g.gkey NOT IN (SELECT gkey FROM _m1) AND g.gkey NOT IN (SELECT gkey FROM _m2) AND g.gkey NOT IN (SELECT gkey FROM _m3);

-- (5) fuzzy against the step-2 practices in the same state.
CREATE OR REPLACE TEMP TABLE _pidx AS
SELECT p.practice_id, p.org_pac_id, p.ind_pac_id, _squash(p.name) AS norm, l.state, coalesce(p.n_clinicians, 0) AS n_clinicians
FROM practices p JOIN (SELECT DISTINCT practice_id, state FROM practice_locations) l USING (practice_id)
WHERE p.source = 'cms_dac';
CREATE OR REPLACE TEMP TABLE _m5 AS
SELECT gkey, practice_id, org_pac_id, ind_pac_id, 'fuzzy_name_state' AS method, CAST(score AS DOUBLE) AS score FROM (
    SELECT g.gkey, p.practice_id, p.org_pac_id, p.ind_pac_id, jaro_winkler_similarity(g.norm, p.norm) AS score,
           row_number() OVER (PARTITION BY g.gkey ORDER BY jaro_winkler_similarity(g.norm, p.norm) DESC, p.n_clinicians DESC, p.practice_id) AS rn
    FROM _grain g JOIN _pidx p ON p.state = g.state
    WHERE g.state IS NOT NULL AND length(g.norm) >= 10 AND g.norm LIKE '% %'
      AND g.gkey NOT IN (SELECT gkey FROM _m1) AND g.gkey NOT IN (SELECT gkey FROM _m2)
      AND g.gkey NOT IN (SELECT gkey FROM _m3) AND g.gkey NOT IN (SELECT gkey FROM _m4)
      AND jaro_winkler_similarity(g.norm, p.norm) >= {fuzzy_min}
) WHERE rn = 1;

CREATE OR REPLACE TEMP TABLE _gm AS
SELECT * FROM _m1 UNION ALL SELECT * FROM _m2 UNION ALL SELECT * FROM _m3 UNION ALL SELECT * FROM _m4 UNION ALL SELECT * FROM _m5;

-- Every organization → a practice id: the matched one, or a new Lantern-only practice.
CREATE OR REPLACE TEMP TABLE _gid AS
SELECT g.gkey, coalesce(m.practice_id, 'lantern:' || left(md5(g.gkey), 12)) AS practice_id,
       m.org_pac_id, m.ind_pac_id, m.method, m.score
FROM _grain g LEFT JOIN _gm m USING (gkey);

UPDATE fhir_endpoints
SET practice_id = x.practice_id, practice_match_method = x.method, practice_match_score = x.score
FROM (SELECT l.endpoint_url, l.org_name, i.practice_id, i.method, i.score FROM _lo l JOIN _gid i USING (gkey)) x
WHERE fhir_endpoints.endpoint_url = x.endpoint_url AND fhir_endpoints.org_name = x.org_name;

-- Per practice: the vendor(s) whose lists name it, and the best match.
CREATE OR REPLACE TEMP TABLE _pv AS
WITH n AS (SELECT i.practice_id, l.vendor, count(*) AS n FROM _lo l JOIN _gid i USING (gkey) GROUP BY 1, 2),
r AS (SELECT *, row_number() OVER (PARTITION BY practice_id ORDER BY n DESC, vendor) AS rn FROM n)
SELECT practice_id, arg_min(vendor, rn) AS vendor, count(*) AS n_vendors,
       string_agg(vendor, '|' ORDER BY rn) AS all_vendors,
       coalesce(max(n) FILTER (WHERE rn = 1) = max(n) FILTER (WHERE rn = 2), FALSE) AS is_tie,
       sum(n) AS n_endpoints
FROM r GROUP BY 1;
CREATE OR REPLACE TEMP TABLE _pm AS
SELECT practice_id, arg_max(method, score) AS method, max(score) AS score FROM _gid WHERE method IS NOT NULL GROUP BY 1;

-- Existing practices: record the Lantern vendor; MIPS stays the vendor of record, Lantern fills a gap.
UPDATE practices
SET lantern_vendor = x.vendor, lantern_n_endpoints = x.n_endpoints,
    lantern_match_method = x.method, lantern_match_score = x.score,
    vendor_source = CASE WHEN practices.vendor IS NULL THEN 'lantern' ELSE practices.vendor_source END,
    n_vendors = CASE WHEN practices.vendor IS NULL THEN x.n_vendors ELSE practices.n_vendors END,
    all_vendors = CASE WHEN practices.vendor IS NULL THEN x.all_vendors ELSE practices.all_vendors END,
    is_tie = CASE WHEN practices.vendor IS NULL THEN x.is_tie ELSE practices.is_tie END,
    vendor = coalesce(practices.vendor, x.vendor)
FROM (SELECT v.*, m.method, m.score FROM _pv v JOIN _pm m USING (practice_id)) x
WHERE practices.practice_id = x.practice_id;

-- New practices: organizations matched to a D&C organisation that is not yet a practice, or matched to nothing.
CREATE OR REPLACE TEMP TABLE _nids AS
SELECT DISTINCT practice_id, org_pac_id, ind_pac_id FROM _gid
WHERE practice_id NOT IN (SELECT practice_id FROM practices);

CREATE OR REPLACE TEMP TABLE _nloc AS
SELECT * FROM ({dac_loc})
UNION ALL
SELECT practice_id, 'lantern:' || left(md5(key), 12), address1, NULL, city, state, zip, NULL,
       0 AS n_clinicians,   -- unknown: the list names no clinicians
       row_number() OVER (PARTITION BY practice_id ORDER BY n DESC, key) AS loc_rank
FROM (
    SELECT i.practice_id, l.address AS address1, l.city, l.state, l.zip5 AS zip,
           coalesce(l.address, '') || '|' || coalesce(l.city, '') || '|' || coalesce(l.state, '') || '|' || coalesce(l.zip5, '') AS key,
           count(*) AS n
    FROM _lo l JOIN _gid i USING (gkey)
    JOIN _nids d ON d.practice_id = i.practice_id AND d.org_pac_id IS NULL AND d.ind_pac_id IS NULL
    WHERE l.zip5 IS NOT NULL OR l.address IS NOT NULL
    GROUP BY 1, 2, 3, 4, 5, 6
);
-- The cache key is precomputed as a column so the lookup is a hash join, not a nested loop.
CREATE OR REPLACE TEMP TABLE _nloc_key AS
SELECT l.*, CASE WHEN l.address1 IS NOT NULL AND l.city IS NOT NULL AND l.state IS NOT NULL THEN {akey} END AS gkey
FROM _nloc l;
CREATE OR REPLACE TEMP TABLE _nloc_geo AS
SELECT l.* EXCLUDE (gkey), coalesce(g.lat, z.lat) AS lat, coalesce(g.lon, z.lon) AS lon,
       CASE WHEN g.lat IS NOT NULL AND g.match_type = 'Exact' THEN 'census_exact'
            WHEN g.lat IS NOT NULL THEN 'census_nonexact'
            WHEN z.lat IS NOT NULL THEN 'zip_centroid'
            ELSE 'none' END AS geo_method
FROM _nloc_key l
LEFT JOIN stg_census_geocode g ON g.address_key = l.gkey AND g.match = 'Match'
LEFT JOIN stg_census_zcta z ON z.zip = left(coalesce(l.zip, ''), 5);

INSERT INTO practice_locations
SELECT practice_id, address_id, address1, address2, city, state, zip, phone, n_clinicians, loc_rank, lat, lon, geo_method
FROM _nloc_geo;

INSERT INTO practices
WITH identity AS ({dac_identity}),
spec AS ({dac_spec}),
lname AS (
    SELECT i.practice_id, arg_max(g.org_name, g.n_endpoints) AS name, any_value(g.source_file_id) AS source_file_id
    FROM _gid i JOIN _grain g USING (gkey) GROUP BY 1
),
loc AS (
    SELECT practice_id, count(*) AS n_locations, count(DISTINCT state) AS n_states,
           arg_min(address1, loc_rank) AS address1, arg_min(address2, loc_rank) AS address2,
           arg_min(city, loc_rank) AS city, arg_min(state, loc_rank) AS state, arg_min(zip, loc_rank) AS zip,
           arg_min(phone, loc_rank) AS phone,
           arg_min(lat, loc_rank) AS lat, arg_min(lon, loc_rank) AS lon, arg_min(geo_method, loc_rank) AS geo_method
    FROM _nloc_geo GROUP BY 1
)
SELECT n.practice_id, 'lantern', n.org_pac_id, n.ind_pac_id, coalesce(d.name, ln.name),
       d.n_members, d.n_clinicians,
       0, 0, NULL, NULL,
       l.address1, l.address2, l.city, l.state, l.zip, l.phone,
       coalesce(l.n_locations, 0), coalesce(l.n_states, 0),
       l.lat, l.lon, coalesce(l.geo_method, 'none'),
       sp.top_specialty, sp.specialty_mix,
       v.vendor, 'lantern', NULL, NULL, v.n_vendors, v.all_vendors, v.is_tie,
       NULL,
       v.vendor, v.n_endpoints, m.method, m.score,
       coalesce(d.source_file_id, ln.source_file_id)
FROM _nids n
JOIN _pv v USING (practice_id)
LEFT JOIN identity d USING (practice_id)
LEFT JOIN lname ln USING (practice_id)
LEFT JOIN spec sp USING (practice_id)
LEFT JOIN loc l USING (practice_id)
LEFT JOIN _pm m USING (practice_id);

DROP TABLE _core; DROP TABLE _match; DROP TABLE _lo; DROP TABLE _grain; DROP TABLE _dorg; DROP TABLE _dname; DROP TABLE _dloc;
DROP TABLE _dnpi; DROP TABLE _known; DROP TABLE _m1; DROP TABLE _m2; DROP TABLE _m3; DROP TABLE _m4; DROP TABLE _pidx; DROP TABLE _m5;
DROP TABLE _gm; DROP TABLE _gid; DROP TABLE _pv; DROP TABLE _pm; DROP TABLE _nids; DROP TABLE _nloc; DROP TABLE _nloc_key; DROP TABLE _nloc_geo;
"#,
        fuzzy_min = PRACTICE_FUZZY_MIN,
        dac_loc = crate::sources::cms_dac::dac_locations_sql(new_ids),
        dac_identity = crate::sources::cms_dac::dac_identity_sql(new_ids),
        dac_spec = crate::sources::cms_dac::dac_specialty_sql(new_ids),
        akey = crate::sources::geocode::address_key_sql("l.address1", "l.city", "l.state", "l.zip"),
    )
}

const DERIVE_SQL: &str = r#"
-- Name normalisation shared by both sides of the match.
CREATE OR REPLACE TEMP MACRO _norm_name(x) AS
    trim(regexp_replace(
        regexp_replace(
            regexp_replace(upper(coalesce(x, '')), '&', ' AND ', 'g'),
            '[^A-Z0-9 ]', ' ', 'g'),
        '\b(INC|LLC|PLLC|LLP|LP|CORP|CORPORATION|THE|LTD|CO|PC|PA|SC|DBA)\b', ' ', 'g'))
;
CREATE OR REPLACE TEMP MACRO _squash(x) AS regexp_replace(_norm_name(x), ' +', ' ', 'g');

-- Full state names (some vendor lists spell them out) → USPS codes.
CREATE OR REPLACE TEMP TABLE _states AS SELECT * FROM (VALUES
 ('ALABAMA','AL'),('ALASKA','AK'),('ARIZONA','AZ'),('ARKANSAS','AR'),('CALIFORNIA','CA'),('COLORADO','CO'),
 ('CONNECTICUT','CT'),('DELAWARE','DE'),('DISTRICT OF COLUMBIA','DC'),('FLORIDA','FL'),('GEORGIA','GA'),
 ('HAWAII','HI'),('IDAHO','ID'),('ILLINOIS','IL'),('INDIANA','IN'),('IOWA','IA'),('KANSAS','KS'),('KENTUCKY','KY'),
 ('LOUISIANA','LA'),('MAINE','ME'),('MARYLAND','MD'),('MASSACHUSETTS','MA'),('MICHIGAN','MI'),('MINNESOTA','MN'),
 ('MISSISSIPPI','MS'),('MISSOURI','MO'),('MONTANA','MT'),('NEBRASKA','NE'),('NEVADA','NV'),('NEW HAMPSHIRE','NH'),
 ('NEW JERSEY','NJ'),('NEW MEXICO','NM'),('NEW YORK','NY'),('NORTH CAROLINA','NC'),('NORTH DAKOTA','ND'),('OHIO','OH'),
 ('OKLAHOMA','OK'),('OREGON','OR'),('PENNSYLVANIA','PA'),('RHODE ISLAND','RI'),('SOUTH CAROLINA','SC'),
 ('SOUTH DAKOTA','SD'),('TENNESSEE','TN'),('TEXAS','TX'),('UTAH','UT'),('VERMONT','VT'),('VIRGINIA','VA'),
 ('WASHINGTON','WA'),('WEST VIRGINIA','WV'),('WISCONSIN','WI'),('WYOMING','WY'),('PUERTO RICO','PR'),('GUAM','GU'),
 ('VIRGIN ISLANDS','VI'),('AMERICAN SAMOA','AS'),('NORTHERN MARIANA ISLANDS','MP')) t(name, code);

-- Endpoint attributes: one row per URL (Lantern lists an endpoint once per list source).
CREATE OR REPLACE TEMP TABLE _ep AS
SELECT lower(rtrim(url, '/')) AS url_key,
       arg_max(url, updated_at)                        AS url,
       arg_max(api_developer_name, updated_at)         AS api_developer_name,
       arg_max(capability_fhir_version, updated_at)    AS fhir_version,
       arg_max(http_response, updated_at)              AS http_response,
       max(updated_at)                                 AS updated_at,
       arg_max(source_file_id, updated_at)             AS source_file_id
FROM stg_lantern_endpoints
GROUP BY 1;

-- Developer per list source (for organization rows that came from a vendor list).
CREATE OR REPLACE TEMP TABLE _list_dev AS
SELECT list_source, arg_max(api_developer_name, c) AS api_developer_name
FROM (SELECT list_source, api_developer_name, count(*) c FROM stg_lantern_endpoints
      WHERE list_source IS NOT NULL AND api_developer_name IS NOT NULL GROUP BY 1, 2)
GROUP BY 1;

-- Candidate rows: (endpoint, org) with canonical vendor.
CREATE OR REPLACE TEMP TABLE _cand AS
SELECT DISTINCT ON (o.url_key, upper(o.org_name))
       coalesce(e.url, o.url)                             AS endpoint_url,
       o.org_name,
       coalesce(e.api_developer_name, ld.api_developer_name) AS developer_name_raw,
       coalesce(m.vendor, coalesce(e.api_developer_name, ld.api_developer_name)) AS vendor,
       coalesce(m.vendor_tier, 'unmapped')                AS vendor_tier,
       e.fhir_version, e.http_response,
       CAST(e.updated_at AS DATE)                         AS observed_at,
       coalesce(e.source_file_id, o.source_file_id)       AS source_file_id,
       o.org_identifier AS npi,
       o.address, o.city,
       coalesce(st.code, CASE WHEN length(o.state) = 2 THEN upper(o.state) END) AS state,
       o.zip,
       _squash(o.org_name)                                AS norm
FROM (SELECT *, lower(rtrim(url, '/')) AS url_key FROM stg_lantern_orgs) o
LEFT JOIN _ep e USING (url_key)
LEFT JOIN _list_dev ld ON ld.list_source = o.list_source
LEFT JOIN ref_vendor_map m
       ON lower(trim(m.developer_name_raw)) = lower(trim(coalesce(e.api_developer_name, ld.api_developer_name)))
LEFT JOIN _states st ON st.name = upper(trim(o.state));

-- Hospital name index (CMS names + enrollment legal/DBA names when loaded).
CREATE OR REPLACE TEMP TABLE _hnames AS
SELECT ccn, state, _squash(name) AS norm, 'exact_name' AS method FROM hospitals WHERE in_cms_hgi
UNION
SELECT ccn, state, _squash(organization_name), 'exact_enrollment_name' FROM hospital_enrollment WHERE organization_name IS NOT NULL
UNION
SELECT ccn, state, _squash(dba_name), 'exact_enrollment_name' FROM hospital_enrollment WHERE dba_name IS NOT NULL;
DELETE FROM _hnames WHERE norm IS NULL OR length(norm) < 4;
CREATE OR REPLACE TEMP TABLE _hname_uniq AS
SELECT norm, count(DISTINCT ccn) AS n_ccn FROM _hnames GROUP BY 1;

-- Match only core-EHR vendors' organizations. Vendors whose endpoint lists are
-- overwhelmingly ambulatory (athenahealth, eClinicalWorks, NextGen, ...) list
-- hospital-owned clinics under the hospital's name ("X Hospital - Outpatient"), which
-- would wrongly tag the hospital's inpatient vendor; those only match by NPI.
CREATE OR REPLACE TEMP TABLE _inpatient_vendors AS SELECT * FROM (VALUES
 ('Epic'),('Oracle Health'),('MEDITECH'),('TruBridge'),('MEDHOST'),('Altera Digital Health'),
 ('Azalea Health'),('Indian Health Service (RPMS)'),('Sabiamed'),('Infomedika'),('MedOne'),('Tenzing Medical')) t(vendor);
CREATE OR REPLACE TEMP TABLE _core AS
SELECT row_number() OVER () AS rid, *, vendor IN (SELECT vendor FROM _inpatient_vendors) AS name_match_ok
FROM _cand WHERE vendor_tier = 'core_ehr';

-- (1) NPI
CREATE OR REPLACE TEMP TABLE _m_npi AS
SELECT c.rid, min(n.ccn) AS ccn, 'npi' AS method, 1.0 AS score
FROM _core c JOIN hospital_npi n ON n.npi = c.npi
GROUP BY c.rid;

-- (2) exact normalised name: same state when known, otherwise only if the name is unique nationally.
CREATE OR REPLACE TEMP TABLE _m_exact AS
SELECT rid, ccn, method, 1.0 AS score FROM (
    SELECT c.rid, h.ccn, h.method,
           row_number() OVER (PARTITION BY c.rid ORDER BY CASE h.method WHEN 'exact_name' THEN 0 ELSE 1 END, h.ccn) AS rn
    FROM _core c
    JOIN _hnames h ON h.norm = c.norm
    LEFT JOIN _hname_uniq u ON u.norm = c.norm
    WHERE c.name_match_ok AND c.rid NOT IN (SELECT rid FROM _m_npi)
      AND ((c.state IS NOT NULL AND h.state = c.state) OR (c.state IS NULL AND u.n_ccn = 1))
) WHERE rn = 1;

-- (3) fuzzy: Jaro-Winkler within state (≥ 0.93) or nationally (≥ 0.96) when no state is known.
CREATE OR REPLACE TEMP TABLE _m_fuzzy AS
SELECT rid, ccn, method, score FROM (
    SELECT c.rid, h.ccn,
           CASE WHEN c.state IS NOT NULL THEN 'fuzzy_name_state' ELSE 'fuzzy_name' END AS method,
           jaro_winkler_similarity(c.norm, h.norm) AS score,
           row_number() OVER (PARTITION BY c.rid ORDER BY jaro_winkler_similarity(c.norm, h.norm) DESC, h.ccn) AS rn
    FROM _core c
    JOIN _hnames h ON (c.state IS NOT NULL AND h.state = c.state) OR c.state IS NULL
    WHERE c.name_match_ok
      AND c.rid NOT IN (SELECT rid FROM _m_npi) AND c.rid NOT IN (SELECT rid FROM _m_exact)
      AND length(c.norm) >= 6
      AND jaro_winkler_similarity(c.norm, h.norm) >= CASE WHEN c.state IS NOT NULL THEN 0.93 ELSE 0.96 END
) WHERE rn = 1;

CREATE OR REPLACE TEMP TABLE _match AS
SELECT * FROM _m_npi UNION ALL SELECT * FROM _m_exact UNION ALL SELECT * FROM _m_fuzzy;

DELETE FROM fhir_endpoints;
INSERT INTO fhir_endpoints
SELECT c.endpoint_url, c.org_name, c.vendor, c.developer_name_raw, c.fhir_version, c.http_response,
       m.ccn, m.method, m.score, NULL, NULL, NULL, c.observed_at, c.source_file_id
FROM _cand c
LEFT JOIN _core k ON k.endpoint_url = c.endpoint_url AND k.org_name = c.org_name
LEFT JOIN _match m ON m.rid = k.rid;

-- _core and _match stay for the practice pass (dropped there).
DROP TABLE _states; DROP TABLE _ep; DROP TABLE _list_dev; DROP TABLE _cand; DROP TABLE _hnames;
DROP TABLE _hname_uniq; DROP TABLE _inpatient_vendors; DROP TABLE _m_npi; DROP TABLE _m_exact; DROP TABLE _m_fuzzy;
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_contained_referenced_and_reverse_referenced_orgs() {
        let b = serde_json::json!({
          "resourceType": "Bundle",
          "entry": [
            {"resource": {"resourceType": "Endpoint", "id": "e1", "address": "https://a.example/fhir",
                          "contained": [{"resourceType": "Organization", "id": "c1", "name": "Alpha Hospital"}],
                          "managingOrganization": {"reference": "#c1"}}},
            {"fullUrl": "https://x/Endpoint/e2",
             "resource": {"resourceType": "Endpoint", "id": "e2", "address": "https://b.example/r4/",
                          "managingOrganization": {"reference": "Organization/o2"}}},
            {"resource": {"resourceType": "Organization", "id": "o2", "name": "Beta Medical Center",
                          "identifier": [{"system": "http://hl7.org/fhir/sid/us-npi", "value": "1234567890"}],
                          "address": [{"line": ["1 Main St"], "city": "Town", "state": "TX", "postalCode": "75001-1234"}],
                          "endpoint": [{"reference": "Endpoint/e2"}]}},
            {"resource": {"resourceType": "Organization", "id": "o3", "name": "*Gamma Clinic",
                          "endpoint": [{"reference": "urn:uuid:e4"}]}},
            {"fullUrl": "urn:uuid:e4", "resource": {"resourceType": "Endpoint", "id": "e4", "address": "https://g.example",
                          "name": "FHIR R4 Endpoint for Gamma Clinic"}},
            {"resource": {"resourceType": "Endpoint", "id": "e5", "address": "https://n.example",
                          "name": "Delta Health - FHIR R4 Base URL"}}
          ]
        });
        let rows = parse_bundle(&b);
        let names: Vec<(String, String)> = rows.iter().map(|r| (r.url.clone(), r.org_name.clone())).collect();
        assert!(names.contains(&("https://a.example/fhir".into(), "Alpha Hospital".into())));
        assert!(names.contains(&("https://b.example/r4/".into(), "Beta Medical Center".into())));
        assert!(names.contains(&("https://g.example".into(), "Gamma Clinic".into())));
        assert!(names.contains(&("https://n.example".into(), "Delta Health".into())));
        let beta = rows.iter().find(|r| r.org_name == "Beta Medical Center").unwrap();
        assert_eq!(beta.npi.as_deref(), Some("1234567890"));
        assert_eq!(beta.state.as_deref(), Some("TX"));
        assert_eq!(beta.zip.as_deref(), Some("75001"));
        // de-duplicated: Gamma appears once even though both passes find it
        assert_eq!(rows.iter().filter(|r| r.org_name == "Gamma Clinic").count(), 1);
    }

    #[test]
    fn bundle_file_names_are_safe() {
        assert_eq!(
            bundle_file_name("https://open.epic.com/Endpoints/R4"),
            "list_open.epic.com_Endpoints_R4.json"
        );
    }
}

#[cfg(test)]
mod sql_dump {
    /// `EHR_ATLAS_DUMP_SQL=<dir> cargo test -p ehr-atlas-pipeline dump_derive_sql` writes the
    /// derive SQL to files so it can be iterated on in the DuckDB CLI.
    #[test]
    fn dump_derive_sql() {
        if let Ok(dir) = std::env::var("EHR_ATLAS_DUMP_SQL") {
            std::fs::write(format!("{dir}/lantern_derive.sql"), super::DERIVE_SQL).unwrap();
            std::fs::write(format!("{dir}/lantern_practice.sql"), super::practice_sql()).unwrap();
        }
    }

    /// `EHR_ATLAS_DB=<duckdb file> cargo test -p ehr-atlas-pipeline find_failing_statement -- --ignored --nocapture`
    /// runs the hospital derive, then the practice pass one statement at a time, and prints the
    /// first statement the bundled DuckDB rejects.
    #[test]
    #[ignore]
    fn find_failing_statement() {
        let Ok(db) = std::env::var("EHR_ATLAS_DB") else { return };
        let conn = duckdb::Connection::open(&db).unwrap();
        let v: String = conn.query_row("SELECT version()", [], |r| r.get(0)).unwrap();
        println!("bundled duckdb {v}");
        conn.execute_batch(super::DERIVE_SQL).unwrap();
        match conn.execute_batch(&super::practice_sql()) {
            Ok(()) => { println!("batch ok"); return; }
            Err(e) => println!("BATCH FAILED: {e}"),
        }
        conn.execute_batch(super::DERIVE_SQL).unwrap();
        for (i, stmt) in super::practice_sql().split(";\n").enumerate() {
            if stmt.trim().is_empty() { continue; }
            if let Err(e) = conn.execute_batch(&format!("{stmt};")) {
                println!("STATEMENT {i} FAILED: {e}\n{stmt}");
                return;
            }
        }
        println!("all statements ok");
    }
}
