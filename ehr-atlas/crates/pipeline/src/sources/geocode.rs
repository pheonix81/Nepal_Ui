//! Hospital coordinates: Census batch geocoder (street-level) with ZCTA centroids as a
//! fallback.
//!
//! * The ZCTA gazetteer zip is downloaded like any other raw file.
//! * Every distinct hospital address (CMS Hospital General Information first, the
//!   Enrollments practice location for CCNs the HGI file lacks) is keyed by a hash of its
//!   normalised form and sent to the Census batch endpoint in chunks of ≤10,000. Each
//!   response CSV is cached under `data/cache/geocode/` and registered in the manifest,
//!   so re-runs only geocode addresses not seen before and `--offline` is fully served
//!   from the cache.
//! * `derive` picks the street match when there is one, else the ZIP centroid.

use crate::config::Paths;
use crate::db::{count, lit};
use crate::download::{download, latest_from_manifest, register_local_file, DownloadRequest};
use anyhow::{bail, ensure, Context, Result};
use duckdb::{params, Connection};
use ehr_atlas_core::manifest::RawFile;
use ehr_atlas_core::sources::{
    SourceId, CENSUS_GEOCODER_BATCH, CENSUS_GEOCODER_BENCHMARK, CENSUS_ZCTA_GAZETTEER_ZIP,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

pub const SOURCE: &str = "census_geocode";
/// The documented limit is 10,000 rows, but batches above a few thousand routinely get
/// a 502 from the geocoder; 1,000 rows takes ~20-60 s and is reliable.
const BATCH_SIZE: usize = 1_000;
const RETRIES: usize = 4;

fn cache_dir(paths: &Paths) -> PathBuf {
    paths.cache.join("geocode")
}

/// Normalised address string and its key. Whitespace is collapsed, case is upper,
/// ZIP is truncated to 5 digits.
pub fn address_key(street: &str, city: &str, state: &str, zip: &str) -> (String, String) {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_uppercase();
    let zip5: String = zip.chars().filter(|c| c.is_ascii_digit()).take(5).collect();
    let input = format!("{}|{}|{}|{}", norm(street), norm(city), norm(state), zip5);
    let key = hex::encode(Sha256::digest(input.as_bytes()))[..16].to_string();
    (key, input)
}

/// The SQL expression that reproduces [`address_key`] for the given column expressions,
/// so derive steps can join staging rows to `stg_census_geocode`.
pub fn address_key_sql(street: &str, city: &str, state: &str, zip: &str) -> String {
    let norm = |e: &str| format!("upper(regexp_replace(trim({e}), '\\s+', ' ', 'g'))");
    format!(
        "left(sha256({s} || '|' || {c} || '|' || {st} || '|' || left(regexp_replace(coalesce({zip}, ''), '[^0-9]', '', 'g'), 5)), 16)",
        s = norm(street),
        c = norm(city),
        st = norm(state),
    )
}

/// Addresses to geocode: (key, street, city, state, zip5), one per distinct address.
/// Hospitals (CMS HGI, else the Enrollments practice location) plus, when the Doctors &
/// Clinicians file and the clinician PUF are loaded, the top locations of every practice.
fn candidate_addresses(conn: &Connection) -> Result<BTreeMap<String, (String, String, String, String)>> {
    let mut sql = String::from(
        r#"
        SELECT address, city, state, zip FROM (
            SELECT address, city, state, zip FROM stg_cms_hospitals
            WHERE address IS NOT NULL AND city IS NOT NULL AND state IS NOT NULL
            UNION ALL
            SELECT e.address1, e.city, e.state, e.zip FROM stg_cms_enrollments e
            WHERE e.address1 IS NOT NULL AND e.city IS NOT NULL AND e.state IS NOT NULL
              AND NOT EXISTS (SELECT 1 FROM stg_cms_hospitals h WHERE h.ccn = e.ccn AND h.address IS NOT NULL)
        "#,
    );
    if crate::sources::cms_dac::stage_practice_locations(conn)? {
        sql.push_str("    UNION ALL\n    ");
        sql.push_str(&crate::sources::cms_dac::geocode_candidates_sql());
        sql.push('\n');
    } else {
        warn!("Doctors & Clinicians file or clinician PUF not loaded; geocoding hospitals only (run `cms-dac` and `clinicians` first for practices)");
    }
    sql.push_str("        )");
    let mut stmt = conn.prepare(&sql)?;
    let mut out = BTreeMap::new();
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        ))
    })?;
    for row in rows {
        let (street, city, state, zip) = row?;
        let (key, _) = address_key(&street, &city, &state, &zip);
        let zip5: String = zip.chars().filter(|c| c.is_ascii_digit()).take(5).collect();
        out.entry(key).or_insert((street, city, state, zip5));
    }
    Ok(out)
}

/// Keys already present in any cached response file.
fn cached_keys(paths: &Paths) -> Result<std::collections::HashSet<String>> {
    let mut keys = std::collections::HashSet::new();
    let dir = cache_dir(paths);
    if !dir.exists() {
        return Ok(keys);
    }
    for entry in std::fs::read_dir(&dir)? {
        let p = entry?.path();
        if p.extension().and_then(|e| e.to_str()) != Some("csv") || p.file_name().and_then(|n| n.to_str()) == Some("gazetteer.txt") {
            continue;
        }
        for rec in parse_response(&p)? {
            keys.insert(rec.key);
        }
    }
    Ok(keys)
}

struct GeoRecord {
    key: String,
    input: String,
    matched: String,      // Match | Tie | No_Match
    match_type: Option<String>,
    matched_address: Option<String>,
    lon: Option<f64>,
    lat: Option<f64>,
    tiger_id: Option<String>,
    side: Option<String>,
}

/// Parse a Census batch response. Rows have 3 columns for No_Match and 8 otherwise.
fn parse_response(path: &Path) -> Result<Vec<GeoRecord>> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_path(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut out = Vec::new();
    for rec in rdr.records() {
        let rec = rec?;
        if rec.len() < 3 {
            continue;
        }
        let get = |i: usize| rec.get(i).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let coords = get(5);
        let (lon, lat) = match coords.as_deref().and_then(|c| c.split_once(',')) {
            Some((x, y)) => (x.trim().parse().ok(), y.trim().parse().ok()),
            None => (None, None),
        };
        out.push(GeoRecord {
            key: get(0).unwrap_or_default(),
            input: get(1).unwrap_or_default(),
            matched: get(2).unwrap_or_else(|| "No_Match".into()),
            match_type: get(3),
            matched_address: get(4),
            lon,
            lat,
            tiger_id: get(6),
            side: get(7),
        });
    }
    Ok(out)
}

/// (a) gazetteer zip, (b) geocode every address not yet cached. Returns the gazetteer
/// file first, then every cached response registered in the manifest.
pub async fn fetch(paths: &Paths, client: &reqwest::Client, offline: bool) -> Result<Vec<RawFile>> {
    let gaz = if offline {
        latest_from_manifest(paths, SourceId::CensusZcta.as_str())?
    } else {
        download(
            paths,
            client,
            DownloadRequest {
                source: SourceId::CensusZcta.as_str(),
                url: CENSUS_ZCTA_GAZETTEER_ZIP,
                file_name: "Gaz_zcta_national.zip",
                upstream_modified: None,
                notes: Some("Census 2024 ZCTA gazetteer (internal-point lat/lon per ZIP)".into()),
            },
        )
        .await?
    };

    let conn = crate::db::open(paths)?;
    let n_h = count(&conn, "SELECT count(*) FROM stg_cms_hospitals")?;
    ensure!(n_h > 0, "stg_cms_hospitals is empty; run `cms-hospitals` first");
    let candidates = candidate_addresses(&conn)?;
    let have = cached_keys(paths)?;
    let todo: Vec<(&String, &(String, String, String, String))> =
        candidates.iter().filter(|(k, _)| !have.contains(*k)).collect();
    info!(addresses = candidates.len(), cached = candidates.len() - todo.len(), to_geocode = todo.len(), "geocode plan");

    if !todo.is_empty() {
        if offline {
            warn!(n = todo.len(), "--offline: addresses not in the cache will fall back to ZIP centroids");
        } else {
            std::fs::create_dir_all(cache_dir(paths))?;
            for (i, chunk) in todo.chunks(BATCH_SIZE).enumerate() {
                let mut body = String::new();
                for (key, (street, city, state, zip)) in chunk {
                    let q = |s: &str| format!("\"{}\"", s.replace('"', "'"));
                    body.push_str(&format!("{},{},{},{},{}\n", key, q(street), q(city), q(state), q(zip)));
                }
                info!(batch = i + 1, rows = chunk.len(), "posting to Census batch geocoder");
                let mut text = String::new();
                for attempt in 1..=RETRIES {
                    let form = reqwest::multipart::Form::new()
                        .text("benchmark", CENSUS_GEOCODER_BENCHMARK)
                        .part(
                            "addressFile",
                            reqwest::multipart::Part::text(body.clone())
                                .file_name("addresses.csv")
                                .mime_str("text/csv")?,
                        );
                    let result = client.post(CENSUS_GEOCODER_BATCH).multipart(form).send().await;
                    match result.and_then(|r| r.error_for_status()) {
                        Ok(resp) => {
                            text = resp.text().await?;
                            break;
                        }
                        Err(e) if attempt < RETRIES => {
                            let wait = 15 * attempt as u64;
                            warn!(batch = i + 1, attempt, error = %e, wait_s = wait, "geocoder request failed; retrying");
                            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        }
                        Err(e) => return Err(e).context("Census batch geocoder"),
                    }
                }
                let sha = hex::encode(Sha256::digest(text.as_bytes()));
                let tmp = cache_dir(paths).join(format!("{}.csv", &sha[..16]));
                std::fs::write(&tmp, &text)?;
                let n_ok = parse_response(&tmp)?.iter().filter(|r| r.matched == "Match").count();
                info!(batch = i + 1, matched = n_ok, path = %tmp.display(), "geocoder response cached");
                if n_ok == 0 && chunk.len() > 10 {
                    bail!("Census geocoder returned no matches for a batch of {}; response starts: {:.200}", chunk.len(), text);
                }
            }
        }
    }

    // Register every cache file in the manifest (idempotent by content hash).
    let mut files = vec![gaz];
    let mut cache_files: Vec<PathBuf> = std::fs::read_dir(cache_dir(paths))
        .map(|d| {
            d.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("csv"))
                .collect()
        })
        .unwrap_or_default();
    cache_files.sort();
    for p in cache_files {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        let rf = register_local_file(
            paths,
            SOURCE,
            &p,
            &format!("geocoder_{name}"),
            Some(format!("Census batch geocoder response, benchmark {CENSUS_GEOCODER_BENCHMARK}")),
        )?;
        files.push(rf);
    }
    Ok(files)
}

/// Extract the gazetteer text file from the zip into the cache and load both staging tables.
pub fn normalize(conn: &Connection, paths: &Paths, files: &[RawFile]) -> Result<()> {
    let gaz = files
        .iter()
        .find(|f| f.source == SourceId::CensusZcta.as_str())
        .context("no gazetteer file among fetched files")?;
    let zip_path = paths.root.join(&gaz.path);
    let txt_path = cache_dir(paths).join("gazetteer.txt");
    std::fs::create_dir_all(cache_dir(paths))?;
    {
        let f = std::fs::File::open(&zip_path)?;
        let mut archive = zip::ZipArchive::new(f)?;
        let idx = (0..archive.len())
            .find(|&i| archive.by_index(i).map(|e| e.name().ends_with(".txt")).unwrap_or(false))
            .context("gazetteer zip has no .txt entry")?;
        let mut entry = archive.by_index(idx)?;
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        std::fs::write(&txt_path, buf)?;
    }
    conn.execute_batch(&format!(
        r#"
        DELETE FROM stg_census_zcta;
        INSERT INTO stg_census_zcta
        SELECT trim(GEOID), TRY_CAST(INTPTLAT AS DOUBLE), TRY_CAST(trim(INTPTLONG) AS DOUBLE), {fid}
        FROM read_csv({p}, delim='\t', header=true, all_varchar=true, normalize_names=false)
        WHERE TRY_CAST(INTPTLAT AS DOUBLE) IS NOT NULL;
        "#,
        p = lit(&txt_path.to_string_lossy()),
        fid = lit(&gaz.id),
    ))?;
    let n_z = count(conn, "SELECT count(*) FROM stg_census_zcta")?;
    ensure!(n_z > 30_000, "gazetteer loaded only {n_z} ZCTAs; expected ~33k");

    conn.execute_batch("DELETE FROM stg_census_geocode")?;
    let mut ins = conn.prepare(
        "INSERT OR REPLACE INTO stg_census_geocode
         (address_key, input_address, match, match_type, matched_address, lat, lon, tiger_line_id, side, geocoded_at, source_file_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?;
    let mut n = 0usize;
    for f in files.iter().filter(|f| f.source == SOURCE) {
        let p = paths.root.join(&f.path);
        let at = f.downloaded_at.format("%Y-%m-%d %H:%M:%S").to_string();
        for r in parse_response(&p)? {
            if r.key.len() != 16 {
                continue;
            }
            ins.execute(params![
                r.key, r.input, r.matched, r.match_type, r.matched_address, r.lat, r.lon, r.tiger_id, r.side, at, f.id
            ])?;
            n += 1;
        }
    }
    let n_geo = count(conn, "SELECT count(*) FROM stg_census_geocode")?;
    let n_match = count(conn, "SELECT count(*) FROM stg_census_geocode WHERE match = 'Match'")?;
    info!(zcta = n_z, geocode_rows = n, geocode_addresses = n_geo, matched = n_match, "geocode staging loaded");
    Ok(())
}

pub fn derive(conn: &Connection) -> Result<()> {
    // Recompute the address key in SQL exactly as `address_key` does in Rust.
    conn.execute_batch(&format!(
        r#"
        CREATE OR REPLACE TEMP TABLE _addr AS
        WITH src AS (
            SELECT ccn, address AS street, city, state, zip, 'cms_hospitals' AS address_source, 1 AS pri
            FROM stg_cms_hospitals WHERE address IS NOT NULL AND city IS NOT NULL AND state IS NOT NULL
            UNION ALL
            SELECT ccn, address1, city, state, zip, 'cms_enrollments', 2
            FROM stg_cms_enrollments WHERE address1 IS NOT NULL AND city IS NOT NULL AND state IS NOT NULL
        ), norm AS (
            SELECT ccn, address_source, pri,
                   upper(regexp_replace(trim(street), '\s+', ' ', 'g')) AS s,
                   upper(regexp_replace(trim(city),   '\s+', ' ', 'g')) AS c,
                   upper(regexp_replace(trim(state),  '\s+', ' ', 'g')) AS st,
                   left(regexp_replace(coalesce(zip, ''), '[^0-9]', '', 'g'), 5) AS zip5,
                   {akey} AS address_key
            FROM src
        )
        SELECT ccn, address_source, pri, s || '|' || c || '|' || st || '|' || zip5 AS input_address,
               address_key, zip5
        FROM norm
        QUALIFY row_number() OVER (PARTITION BY ccn ORDER BY pri) = 1;

        DELETE FROM hospital_geo;
        INSERT INTO hospital_geo
        SELECT h.ccn,
               coalesce(g.lat, z.lat) AS lat,
               coalesce(g.lon, z.lon) AS lon,
               CASE WHEN g.lat IS NOT NULL AND g.match_type = 'Exact' THEN 'census_exact'
                    WHEN g.lat IS NOT NULL THEN 'census_nonexact'
                    WHEN z.lat IS NOT NULL THEN 'zip_centroid'
                    ELSE 'none' END AS method,
               a.address_source,
               a.input_address,
               g.matched_address
        FROM hospitals h
        LEFT JOIN _addr a ON a.ccn = h.ccn
        LEFT JOIN stg_census_geocode g ON g.address_key = a.address_key AND g.match = 'Match'
        LEFT JOIN stg_census_zcta z ON z.zip = coalesce(a.zip5, left(regexp_replace(coalesce(h.zip, ''), '[^0-9]', '', 'g'), 5));
        DROP TABLE _addr;
        "#,
        akey = address_key_sql("street", "city", "state", "zip"),
    ))?;
    let mut stmt = conn.prepare(
        "SELECT method, count(*) FROM hospital_geo g JOIN hospitals h USING (ccn) WHERE h.in_cms_hgi GROUP BY 1 ORDER BY 2 DESC",
    )?;
    let rows: Vec<(String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let total: i64 = rows.iter().map(|r| r.1).sum();
    for (m, n) in &rows {
        info!(method = m, hospitals = n, pct = format!("{:.1}", 100.0 * *n as f64 / total.max(1) as f64), "hospital_geo by method (CMS hospitals)");
    }
    let n = count(conn, "SELECT count(*) FROM hospital_geo")?;
    info!(hospital_geo = n, "geo derived");
    Ok(())
}
