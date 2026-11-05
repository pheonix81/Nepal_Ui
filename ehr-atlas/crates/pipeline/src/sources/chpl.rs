//! ONC CHPL API client: CEHRT ID → certified products → developer.
//!
//! * Endpoint: `GET https://chpl.healthit.gov/rest/certification_ids/{cehrtId}` with an
//!   `API-Key` header. Keys are free: register at https://chpl.healthit.gov/#/resources/api.
//! * Every response (including 404s and errors) is cached at
//!   `data/cache/chpl/<CEHRT_ID>.json`. The cache is the source of truth: the staging
//!   table is rebuilt from cache files on every run, so parser fixes never require
//!   re-hitting the API. Delete a cache file to force a refetch.
//! * Requests are serialised with a minimum interval and retried with backoff on 429/5xx.
//!
//! Only CEHRT IDs that ONC has not already linked are fetched by default (`--all`
//! fetches every ID seen, useful to cross-check ONC's linkage).

use crate::config::Paths;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use duckdb::{params, Connection};
use ehr_atlas_core::sources::CHPL_API_BASE;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use tracing::{info, warn};

#[derive(Debug, Serialize, Deserialize)]
pub struct CachedResponse {
    pub cehrt_id: String,
    pub url: String,
    pub fetched_at: DateTime<Utc>,
    pub http_status: u16,
    /// `ok` | `not_found` | `error`
    pub status: String,
    pub body: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ChplProduct {
    pub chpl_id: Option<String>,
    pub chpl_product_db_id: Option<String>,
    pub developer_name: Option<String>,
    pub product_name: Option<String>,
    pub version: Option<String>,
    pub edition_year: Option<String>,
}

pub struct ChplClient {
    http: reqwest::Client,
    api_key: Option<String>,
    cache_dir: PathBuf,
    min_interval: Duration,
    last_request: Option<tokio::time::Instant>,
}

impl ChplClient {
    pub fn new(
        paths: &Paths,
        http: reqwest::Client,
        api_key: Option<String>,
        min_interval_ms: u64,
    ) -> Result<Self> {
        let cache_dir = paths.cache.join("chpl");
        std::fs::create_dir_all(&cache_dir)?;
        Ok(Self {
            http,
            api_key,
            cache_dir,
            min_interval: Duration::from_millis(min_interval_ms),
            last_request: None,
        })
    }

    pub fn has_key(&self) -> bool {
        self.api_key.is_some()
    }

    fn cache_path(&self, cehrt_id: &str) -> PathBuf {
        // CEHRT IDs are alphanumeric; guard anyway.
        let safe: String = cehrt_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        self.cache_dir.join(format!("{safe}.json"))
    }

    pub fn read_cache(&self, cehrt_id: &str) -> Result<Option<CachedResponse>> {
        let p = self.cache_path(cehrt_id);
        if !p.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&p)?;
        Ok(Some(serde_json::from_str(&text).with_context(|| {
            format!("parsing cache {}", p.display())
        })?))
    }

    pub fn all_cached(&self) -> Result<Vec<CachedResponse>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.cache_dir)? {
            let p = entry?.path();
            if p.extension().and_then(|e| e.to_str()) == Some("json") {
                let text = std::fs::read_to_string(&p)?;
                match serde_json::from_str::<CachedResponse>(&text) {
                    Ok(c) => out.push(c),
                    Err(e) => {
                        warn!(path = %p.display(), error = %e, "skipping unreadable cache file")
                    }
                }
            }
        }
        Ok(out)
    }

    /// Fetch (or reuse cached) response for one CEHRT ID.
    pub async fn resolve(&mut self, cehrt_id: &str) -> Result<CachedResponse> {
        if let Some(c) = self.read_cache(cehrt_id)? {
            return Ok(c);
        }
        let key = self.api_key.as_deref().context("CHPL_API_KEY not set")?;
        let url = format!("{CHPL_API_BASE}/certification_ids/{cehrt_id}");

        let mut attempt = 0u32;
        loop {
            if let Some(last) = self.last_request {
                let elapsed = last.elapsed();
                if elapsed < self.min_interval {
                    tokio::time::sleep(self.min_interval - elapsed).await;
                }
            }
            self.last_request = Some(tokio::time::Instant::now());
            let resp = self
                .http
                .get(&url)
                .header("API-Key", key)
                .header("Accept", "application/json")
                .send()
                .await;
            attempt += 1;
            match resp {
                Ok(r) => {
                    let code = r.status().as_u16();
                    let text = r.text().await.unwrap_or_default();
                    let body: serde_json::Value = serde_json::from_str(&text)
                        .unwrap_or(serde_json::Value::String(text.clone()));
                    let status = match code {
                        200 => "ok",
                        404 => "not_found",
                        429 | 500..=599 if attempt < 4 => {
                            let wait = Duration::from_secs(2u64.pow(attempt));
                            warn!(cehrt_id, code, ?wait, "CHPL transient error; retrying");
                            tokio::time::sleep(wait).await;
                            continue;
                        }
                        401 | 403 => {
                            anyhow::bail!("CHPL rejected the API key (HTTP {code}): {text}")
                        }
                        _ => "error",
                    };
                    let cached = CachedResponse {
                        cehrt_id: cehrt_id.to_string(),
                        url: url.clone(),
                        fetched_at: Utc::now(),
                        http_status: code,
                        status: status.to_string(),
                        body,
                    };
                    std::fs::write(
                        self.cache_path(cehrt_id),
                        serde_json::to_string_pretty(&cached)?,
                    )?;
                    info!(cehrt_id, code, status, "CHPL fetched");
                    return Ok(cached);
                }
                Err(e) if attempt < 4 => {
                    warn!(cehrt_id, error = %e, "CHPL request failed; retrying");
                    tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
                }
                Err(e) => return Err(e).context("CHPL request"),
            }
        }
    }
}

/// Lenient parse of the `certification_ids/{id}` payload. CHPL returns
/// `{ "ehrCertificationId", "year", "criteria", "cqms", "products": [ {id, chplProductNumber,
/// name, version, vendor, acb, year, curesUpdate, ...} ] }` (shape verified against the live
/// API on 2026-09-26; `vendor` is the developer name). `developer` as a string or as an
/// object with `name` is also accepted for forward compatibility. Unknown shapes yield zero products rather than an
/// error so the raw body can be inspected in the cache.
pub fn parse_products(body: &serde_json::Value) -> Vec<ChplProduct> {
    let Some(products) = body.get("products").and_then(|p| p.as_array()) else {
        return Vec::new();
    };
    products
        .iter()
        .map(|p| {
            let s = |k: &str| p.get(k).and_then(|v| v.as_str()).map(str::to_string);
            let num_or_str = |k: &str| {
                p.get(k).and_then(|v| {
                    v.as_i64()
                        .map(|n| n.to_string())
                        .or_else(|| v.as_str().map(str::to_string))
                })
            };
            let developer = match p.get("developer") {
                Some(serde_json::Value::String(d)) => Some(d.clone()),
                Some(serde_json::Value::Object(o)) => {
                    o.get("name").and_then(|v| v.as_str()).map(str::to_string)
                }
                // Live payloads (verified 2026-09-26) use `vendor` as a plain string.
                _ => s("vendor").or_else(|| s("developerName")),
            };
            ChplProduct {
                chpl_id: s("chplProductNumber").or_else(|| s("productNumber")),
                chpl_product_db_id: num_or_str("id"),
                developer_name: developer,
                product_name: s("name").or_else(|| s("productName")),
                version: s("version"),
                edition_year: num_or_str("year").or_else(|| s("edition")).or_else(|| {
                    body.get("year")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                }),
            }
        })
        .collect()
}

/// CEHRT IDs present in staging that ONC has not linked to a developer.
pub fn unresolved_ids(conn: &Connection) -> Result<Vec<String>> {
    let sql = r#"
        WITH needed AS (
            SELECT DISTINCT cehrt_id FROM stg_cms_pi WHERE cehrt_id IS NOT NULL
            UNION
            SELECT DISTINCT cehrt_id FROM stg_onc_linkage WHERE cehrt_id IS NOT NULL
        ),
        by_onc AS (
            SELECT cehrt_id FROM stg_onc_linkage WHERE cehrt_id IS NOT NULL
            GROUP BY 1 HAVING count(*) FILTER (WHERE developer_name_raw IS NULL) = 0
        )
        SELECT cehrt_id FROM needed WHERE cehrt_id NOT IN (SELECT cehrt_id FROM by_onc) ORDER BY 1"#;
    Ok(conn
        .prepare(sql)?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?)
}

pub fn all_ids(conn: &Connection) -> Result<Vec<String>> {
    let sql = r#"
        SELECT DISTINCT cehrt_id FROM stg_cms_pi WHERE cehrt_id IS NOT NULL
        UNION SELECT DISTINCT cehrt_id FROM stg_onc_linkage WHERE cehrt_id IS NOT NULL ORDER BY 1"#;
    Ok(conn
        .prepare(sql)?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?)
}

/// Rebuild `stg_chpl_cehrt` from every cache file on disk.
pub fn load_cache_into_staging(conn: &Connection, client: &ChplClient) -> Result<(usize, usize)> {
    conn.execute_batch("DELETE FROM stg_chpl_cehrt")?;
    let mut stmt = conn.prepare(
        "INSERT INTO stg_chpl_cehrt (cehrt_id, chpl_id, chpl_product_db_id, developer_name_raw, product_name_raw,
                                     version_raw, edition_year, fetched_at, status)
         VALUES (?, ?, ?, ?, ?, ?, ?, CAST(? AS TIMESTAMP), ?)",
    )?;
    let (mut n_ids, mut n_rows) = (0usize, 0usize);
    for c in client.all_cached()? {
        n_ids += 1;
        let fetched = c.fetched_at.format("%Y-%m-%d %H:%M:%S").to_string();
        let products = if c.status == "ok" {
            parse_products(&c.body)
        } else {
            Vec::new()
        };
        if products.is_empty() {
            let status = if c.status == "ok" {
                "ok_no_products"
            } else {
                c.status.as_str()
            };
            stmt.execute(params![
                c.cehrt_id,
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
                fetched,
                status
            ])?;
            n_rows += 1;
            continue;
        }
        for p in products {
            stmt.execute(params![
                c.cehrt_id,
                p.chpl_id,
                p.chpl_product_db_id,
                p.developer_name,
                p.product_name,
                p.version,
                p.edition_year,
                fetched,
                "ok"
            ])?;
            n_rows += 1;
        }
    }
    Ok((n_ids, n_rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_string_and_object_developers() {
        let body = serde_json::json!({
            "ehrCertificationId": "0015CAN28DKT47C", "year": "2015",
            "products": [
                {"id": 10942, "chplProductNumber": "15.04.04.1447.Epic.IN.23.1.220713", "name": "EpicCare Inpatient Base", "version": "May 2023", "developer": "Epic Systems Corporation"},
                {"id": "11", "chplProductNumber": "15.04.04.2880.Ora1.01.0.1.230101", "name": "Millennium", "developer": {"name": "Oracle Health", "id": 2880}},
                {"id": 11121, "chplProductNumber": "15.04.04.2790.Open.02.02.1.221222", "name": "CareVue", "version": "2.1", "vendor": "CareCloud, Inc.", "acb": "Drummond Group", "year": "2015"}
            ]
        });
        let p = parse_products(&body);
        assert_eq!(p.len(), 3);
        assert_eq!(p[2].developer_name.as_deref(), Some("CareCloud, Inc."));
        assert_eq!(p[2].product_name.as_deref(), Some("CareVue"));
        assert_eq!(p[0].chpl_product_db_id.as_deref(), Some("10942"));
        assert_eq!(
            p[0].developer_name.as_deref(),
            Some("Epic Systems Corporation")
        );
        assert_eq!(p[0].edition_year.as_deref(), Some("2015"));
        assert_eq!(p[1].chpl_product_db_id.as_deref(), Some("11"));
        assert_eq!(p[1].developer_name.as_deref(), Some("Oracle Health"));
    }

    #[test]
    fn unknown_shape_yields_no_products() {
        assert!(parse_products(&serde_json::json!({"error": "nope"})).is_empty());
        assert!(parse_products(&serde_json::Value::String("html".into())).is_empty());
    }
}
