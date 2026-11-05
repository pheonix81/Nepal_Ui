//! ONC CHPL certified-product catalog via `GET /rest/search/v3` (paged, needs `API-Key`).
//!
//! The bulk `/rest/download` endpoint currently answers HTTP 500, so the search endpoint
//! is paged instead (~11.8k listings, 100 per page). Every page is cached verbatim at
//! `data/cache/chpl_products/page_<N>.json` and `stg_chpl_products` is rebuilt from the
//! cache on every run, so parser fixes never require re-hitting the API. `--refresh`
//! re-fetches every page; delete a page file to refetch just that one.
//!
//! Field names verified against the live API on 2026-09-26: `chplProductNumber`, `id`,
//! `developer.name`, `product.name`, `version.name`, `edition.name` (null for some Cures
//! Update listings; `curesUpdate` is a separate boolean), `certificationDate`,
//! `certificationStatus.name`, `certificationBody.name`, `criteriaMet[].number`,
//! `cqmsMet[]`, `surveillanceCount`, `openSurveillanceNonConformityCount`,
//! `closedSurveillanceNonConformityCount`, `decertificationDate`.

use crate::config::Paths;
use crate::db::count;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use duckdb::{params, Connection};
use ehr_atlas_core::sources::CHPL_API_BASE;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use tracing::{info, warn};

pub const SOURCE: &str = "chpl_products";
const PAGE_SIZE: u32 = 100;
const G10: &str = "170.315 (g)(10)";

#[derive(Debug, Serialize, Deserialize)]
struct CachedPage {
    page: u32,
    url: String,
    fetched_at: DateTime<Utc>,
    body: Value,
}

fn cache_dir(paths: &Paths) -> PathBuf {
    paths.cache.join("chpl_products")
}

fn page_path(paths: &Paths, page: u32) -> PathBuf {
    cache_dir(paths).join(format!("page_{page:04}.json"))
}

fn read_page(paths: &Paths, page: u32) -> Result<Option<CachedPage>> {
    let p = page_path(paths, page);
    if !p.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&p)?;
    Ok(Some(serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))?))
}

async fn fetch_page(
    client: &reqwest::Client,
    api_key: &str,
    page: u32,
    min_interval: Duration,
    last_request: &mut Option<tokio::time::Instant>,
) -> Result<CachedPage> {
    let url = format!("{CHPL_API_BASE}/search/v3?pageSize={PAGE_SIZE}&pageNumber={page}");
    let mut attempt = 0u32;
    loop {
        if let Some(last) = *last_request {
            let elapsed = last.elapsed();
            if elapsed < min_interval {
                tokio::time::sleep(min_interval - elapsed).await;
            }
        }
        *last_request = Some(tokio::time::Instant::now());
        attempt += 1;
        let resp = client
            .get(&url)
            .header("API-Key", api_key)
            .header("Accept", "application/json")
            .send()
            .await;
        match resp {
            Ok(r) => {
                let code = r.status().as_u16();
                let text = r.text().await.unwrap_or_default();
                match code {
                    200 => {
                        let body: Value = serde_json::from_str(&text)
                            .with_context(|| format!("CHPL page {page}: body is not JSON"))?;
                        anyhow::ensure!(
                            body.get("results").and_then(Value::as_array).is_some(),
                            "CHPL page {page}: no results array: {}",
                            &text[..text.len().min(200)]
                        );
                        return Ok(CachedPage { page, url, fetched_at: Utc::now(), body });
                    }
                    429 | 500..=599 if attempt < 5 => {
                        let wait = Duration::from_secs(2u64.pow(attempt));
                        warn!(page, code, ?wait, "CHPL transient error; retrying");
                        tokio::time::sleep(wait).await;
                    }
                    401 | 403 => anyhow::bail!("CHPL rejected the API key (HTTP {code}): {text}"),
                    _ => anyhow::bail!("CHPL page {page}: HTTP {code}: {}", &text[..text.len().min(300)]),
                }
            }
            Err(e) if attempt < 5 => {
                warn!(page, error = %e, "CHPL request failed; retrying");
                tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
            }
            Err(e) => return Err(e).context("CHPL request"),
        }
    }
}

/// Fetch every search page (unless cached / offline), then load the cache into
/// `stg_chpl_products`.
pub async fn sync(
    conn: &Connection,
    paths: &Paths,
    client: &reqwest::Client,
    api_key: Option<&str>,
    offline: bool,
    refresh: bool,
    min_interval_ms: u64,
) -> Result<()> {
    std::fs::create_dir_all(cache_dir(paths))?;
    let min_interval = Duration::from_millis(min_interval_ms);
    let mut last_request = None;
    let can_fetch = !offline && api_key.is_some();

    // Page 0 tells us how many pages there are.
    let first = match read_page(paths, 0)? {
        Some(p) if !refresh => Some(p),
        _ if can_fetch => {
            let p = fetch_page(client, api_key.unwrap(), 0, min_interval, &mut last_request).await?;
            std::fs::write(page_path(paths, 0), serde_json::to_string(&p)?)?;
            Some(p)
        }
        cached => cached,
    };
    let mut missing: Vec<u32> = Vec::new();
    if let Some(first) = first {
        let total = first.body["recordCount"].as_u64().unwrap_or(0) as u32;
        let n_pages = total.div_ceil(PAGE_SIZE).max(1);
        info!(listings = total, pages = n_pages, "CHPL catalog size");
        for page in 1..n_pages {
            let cached = read_page(paths, page)?;
            if cached.is_some() && !refresh {
                continue;
            }
            if !can_fetch {
                missing.push(page);
                continue;
            }
            let p = fetch_page(client, api_key.unwrap(), page, min_interval, &mut last_request).await?;
            std::fs::write(page_path(paths, page), serde_json::to_string(&p)?)?;
            if page % 10 == 0 {
                info!(page, of = n_pages, "CHPL pages fetched");
            }
        }
    } else {
        missing.push(0);
    }

    let (n_pages, n_rows) = load_cache_into_staging(conn, paths)?;
    info!(cached_pages = n_pages, staged_rows = n_rows, "stg_chpl_products loaded from cache");
    if !missing.is_empty() {
        warn!(
            "{} CHPL search page(s) are not cached but {}. Get a free key at \
             https://chpl.healthit.gov/#/resources/api (register with name + email; the key is emailed), then re-run:\n  CHPL_API_KEY=... ehr-atlas chpl-products",
            missing.len(),
            if offline { "--offline was set" } else { "CHPL_API_KEY is not set" }
        );
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Listing {
    pub chpl_id: String,
    pub db_id: Option<String>,
    pub developer: Option<String>,
    pub product: Option<String>,
    pub version: Option<String>,
    pub edition: Option<String>,
    pub certification_date: Option<String>,
    pub status: Option<String>,
    pub acb: Option<String>,
    pub criteria: Vec<String>,
    pub has_g10: bool,
    pub cqm_count: i64,
    pub surveillance_count: i64,
    pub open_nc: i64,
    pub closed_nc: i64,
    pub decertification_date: Option<String>,
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::trim).filter(|x| !x.is_empty()).map(str::to_string)
}
fn name_of(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|o| s(o, "name"))
}
fn i(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}

pub fn parse_listing(v: &Value) -> Option<Listing> {
    let chpl_id = s(v, "chplProductNumber")?;
    let criteria: Vec<String> = v
        .get("criteriaMet")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|c| s(c, "number")).collect())
        .unwrap_or_default();
    let mut edition = name_of(v, "edition");
    if v.get("curesUpdate").and_then(Value::as_bool) == Some(true) {
        edition = Some(match edition {
            Some(e) => format!("{e} Cures Update"),
            None => "2015 Cures Update".to_string(),
        });
    }
    Some(Listing {
        db_id: v.get("id").and_then(|x| x.as_i64().map(|n| n.to_string()).or_else(|| x.as_str().map(str::to_string))),
        developer: name_of(v, "developer"),
        product: name_of(v, "product"),
        version: name_of(v, "version"),
        edition,
        certification_date: s(v, "certificationDate"),
        status: name_of(v, "certificationStatus"),
        acb: name_of(v, "certificationBody"),
        has_g10: criteria.iter().any(|c| c == G10),
        cqm_count: v.get("cqmsMet").and_then(Value::as_array).map(|a| a.len() as i64).unwrap_or(0),
        surveillance_count: i(v, "surveillanceCount"),
        open_nc: i(v, "openSurveillanceNonConformityCount"),
        closed_nc: i(v, "closedSurveillanceNonConformityCount"),
        decertification_date: s(v, "decertificationDate"),
        criteria,
        chpl_id,
    })
}

/// Rebuild `stg_chpl_products` from every cached page (newest fetch wins per listing).
pub fn load_cache_into_staging(conn: &Connection, paths: &Paths) -> Result<(usize, usize)> {
    conn.execute_batch("DELETE FROM stg_chpl_products")?;
    let mut entries: Vec<PathBuf> = std::fs::read_dir(cache_dir(paths))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    entries.sort();
    let mut stmt = conn.prepare(
        "INSERT INTO stg_chpl_products (chpl_id, chpl_product_db_id, developer_name_raw, product_name_raw, version_raw,
            edition, certification_date, certification_status, certification_body, criteria_met, has_g10_fhir_api,
            cqm_count, surveillance_count, open_nonconformity_count, closed_nonconformity_count, decertification_date, fetched_at)
         VALUES (?, ?, ?, ?, ?, ?, TRY_CAST(? AS DATE), ?, ?, ?, ?, ?, ?, ?, ?, TRY_CAST(? AS DATE), CAST(? AS TIMESTAMP))",
    )?;
    let mut n_pages = 0usize;
    for p in entries {
        let text = std::fs::read_to_string(&p)?;
        let page: CachedPage = match serde_json::from_str(&text) {
            Ok(c) => c,
            Err(e) => {
                warn!(path = %p.display(), error = %e, "skipping unreadable cache page");
                continue;
            }
        };
        n_pages += 1;
        let fetched = page.fetched_at.format("%Y-%m-%d %H:%M:%S").to_string();
        for r in page.body["results"].as_array().map(|a| a.as_slice()).unwrap_or_default() {
            let Some(l) = parse_listing(r) else { continue };
            stmt.execute(params![
                l.chpl_id,
                l.db_id,
                l.developer,
                l.product,
                l.version,
                l.edition,
                l.certification_date,
                l.status,
                l.acb,
                l.criteria.join("|"),
                l.has_g10,
                l.cqm_count,
                l.surveillance_count,
                l.open_nc,
                l.closed_nc,
                l.decertification_date,
                fetched,
            ])?;

        }
    }
    // A listing can appear on two pages when the catalog shifts between fetches: keep the newest.
    conn.execute_batch(
        r#"
        CREATE OR REPLACE TEMP TABLE _cp AS
        SELECT * FROM stg_chpl_products
        QUALIFY row_number() OVER (PARTITION BY chpl_id ORDER BY fetched_at DESC) = 1;
        DELETE FROM stg_chpl_products;
        INSERT INTO stg_chpl_products SELECT * FROM _cp;
        DROP TABLE _cp;
        "#,
    )?;
    Ok((n_pages, count(conn, "SELECT count(*) FROM stg_chpl_products")? as usize))
}

pub fn derive(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        DELETE FROM chpl_products;
        INSERT INTO chpl_products
        SELECT s.chpl_id, s.chpl_product_db_id, s.developer_name_raw,
               coalesce(m.vendor, s.developer_name_raw) AS vendor,
               s.product_name_raw, s.version_raw, s.edition, s.certification_date, s.certification_status,
               s.certification_body, s.criteria_met, s.has_g10_fhir_api, s.cqm_count, s.surveillance_count,
               s.open_nonconformity_count, s.closed_nonconformity_count, s.decertification_date, s.fetched_at
        FROM stg_chpl_products s
        LEFT JOIN ref_vendor_map m ON lower(trim(m.developer_name_raw)) = lower(trim(s.developer_name_raw));
        "#,
    )?;
    let n = count(conn, "SELECT count(*) FROM chpl_products")?;
    let covered = count(
        conn,
        "SELECT count(DISTINCT e.chpl_id) FROM hospital_ehr e JOIN chpl_products p USING (chpl_id)",
    )?;
    let total = count(conn, "SELECT count(DISTINCT chpl_id) FROM hospital_ehr WHERE chpl_id IS NOT NULL")?;
    info!(rows = n, hospital_ehr_chpl_ids_covered = covered, of = total, "chpl_products derived");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_search_result() {
        let v = serde_json::json!({
            "id": 9261, "chplProductNumber": "15.02.02.3007.A056.01.00.0.180214",
            "developer": {"id": 2008, "name": "Health Metrics System, Inc"},
            "product": {"id": 2839, "name": "SQI"}, "version": {"id": 7169, "name": "v4.6.9.25"},
            "edition": null, "curesUpdate": true,
            "certificationDate": "2018-02-14", "certificationStatus": {"id": 3, "name": "Withdrawn by Developer"},
            "certificationBody": {"id": 1, "name": "UL LLC"},
            "criteriaMet": [{"id": 53, "number": "170.315 (g)(4)"}, {"id": 1, "number": "170.315 (g)(10)"}],
            "cqmsMet": [{"id": 1}, {"id": 2}],
            "surveillanceCount": 1, "openSurveillanceNonConformityCount": 0, "closedSurveillanceNonConformityCount": 2,
            "decertificationDate": "2021-07-19"
        });
        let l = parse_listing(&v).unwrap();
        assert_eq!(l.db_id.as_deref(), Some("9261"));
        assert_eq!(l.edition.as_deref(), Some("2015 Cures Update"));
        assert!(l.has_g10);
        assert_eq!(l.cqm_count, 2);
        assert_eq!(l.closed_nc, 2);
        assert_eq!(l.status.as_deref(), Some("Withdrawn by Developer"));
        assert_eq!(l.criteria.len(), 2);
    }
}
