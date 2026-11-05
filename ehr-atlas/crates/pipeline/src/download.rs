//! HTTP download with SHA-256 hashing and manifest bookkeeping.
//!
//! Files land in `data/raw/<source>/<YYYY-MM-DD>_<name>`. A download is skipped when a
//! manifest entry for the same source already has the same content hash (we still fetch
//! to discover that, unless `--offline` is set, in which case the latest manifest entry
//! is reused). Bodies are streamed to a temp file while hashing, so multi-hundred-MB
//! files (cost reports, service areas, clinician PUFs) never sit in memory.

use crate::config::Paths;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use ehr_atlas_core::manifest::{Manifest, RawFile};
use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use tracing::info;

pub fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!(
            "ehr-atlas/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/alex-evans/ehr-atlas)"
        ))
        .timeout(std::time::Duration::from_secs(1800))
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()?)
}

pub struct DownloadRequest<'a> {
    pub source: &'a str,
    pub url: &'a str,
    /// Basename to store the file under (date prefix is added).
    pub file_name: &'a str,
    pub upstream_modified: Option<String>,
    pub notes: Option<String>,
}

/// Fetch `req.url`, hash it, write it under `data/raw/<source>/`, and upsert the manifest.
/// Returns the manifest entry. If the same bytes were already recorded for this source,
/// the previous entry (and file) is reused and no duplicate is written.
pub async fn download(
    paths: &Paths,
    client: &reqwest::Client,
    req: DownloadRequest<'_>,
) -> Result<RawFile> {
    download_with(paths, client, req, |b| b).await
}

/// Like [`download`] but lets the caller adjust the request (extra headers, query).
pub async fn download_with(
    paths: &Paths,
    client: &reqwest::Client,
    req: DownloadRequest<'_>,
    customize: impl FnOnce(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
) -> Result<RawFile> {
    info!(source = req.source, url = req.url, "downloading");
    let mut resp = customize(client.get(req.url))
        .send()
        .await?
        .error_for_status()
        .with_context(|| format!("GET {}", req.url))?;

    let dir = paths.raw.join(req.source);
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!(".{}.part", req.file_name));
    let mut out = std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    let mut hasher = Sha256::new();
    let mut n: u64 = 0;
    while let Some(chunk) = resp.chunk().await? {
        hasher.update(&chunk);
        out.write_all(&chunk)?;
        n += chunk.len() as u64;
    }
    out.flush()?;
    drop(out);
    let sha256 = hex::encode(hasher.finalize());

    let mut manifest = Manifest::load(&paths.manifest)?;
    let id = format!("{}:{}", req.source, &sha256[..12]);
    if let Some(existing) = manifest.files.iter().find(|f| f.id == id) {
        if paths.root.join(&existing.path).exists() {
            info!(id, "unchanged upstream; reusing existing raw file");
            std::fs::remove_file(&tmp).ok();
            return Ok(existing.clone());
        }
    }

    let file_path: PathBuf = dir.join(format!(
        "{}_{}",
        Utc::now().format("%Y-%m-%d"),
        req.file_name
    ));
    std::fs::rename(&tmp, &file_path)
        .with_context(|| format!("moving {} to {}", tmp.display(), file_path.display()))?;

    let rf = RawFile {
        id,
        source: req.source.to_string(),
        url: req.url.to_string(),
        path: paths.relative(&file_path),
        sha256,
        bytes: n,
        downloaded_at: Utc::now(),
        upstream_modified: req.upstream_modified,
        notes: req.notes,
    };
    manifest.upsert(rf.clone());
    manifest.save(&paths.manifest)?;
    info!(path = %rf.path.display(), bytes = rf.bytes, "saved");
    Ok(rf)
}

/// Register a file that was obtained by hand (e.g. AHRQ, which blocks scripted
/// downloads): copy it under `data/raw/<source>/`, hash it, and upsert the manifest.
pub fn register_local_file(
    paths: &Paths,
    source: &str,
    local: &Path,
    file_name: &str,
    notes: Option<String>,
) -> Result<RawFile> {
    let bytes = std::fs::read(local).with_context(|| format!("reading {}", local.display()))?;
    let sha256 = hex::encode(Sha256::digest(&bytes));
    let mut manifest = Manifest::load(&paths.manifest)?;
    let id = format!("{source}:{}", &sha256[..12]);
    if let Some(existing) = manifest.files.iter().find(|f| f.id == id) {
        if paths.root.join(&existing.path).exists() {
            info!(id, "already registered; reusing");
            return Ok(existing.clone());
        }
    }
    let dir = paths.raw.join(source);
    std::fs::create_dir_all(&dir)?;
    let file_path = dir.join(format!("{}_{}", Utc::now().format("%Y-%m-%d"), file_name));
    std::fs::write(&file_path, &bytes)?;
    let rf = RawFile {
        id,
        source: source.to_string(),
        url: format!("file://{}", local.canonicalize().unwrap_or(local.to_path_buf()).display()),
        path: paths.relative(&file_path),
        sha256,
        bytes: bytes.len() as u64,
        downloaded_at: Utc::now(),
        upstream_modified: None,
        notes,
    };
    manifest.upsert(rf.clone());
    manifest.save(&paths.manifest)?;
    info!(path = %rf.path.display(), bytes = rf.bytes, "registered local file");
    Ok(rf)
}

/// Offline mode: reuse the most recent manifest entry for a source.
pub fn latest_from_manifest(paths: &Paths, source: &str) -> Result<RawFile> {
    let manifest = Manifest::load(&paths.manifest)?;
    match manifest.latest(source) {
        Some(f) if paths.root.join(&f.path).exists() => Ok(f.clone()),
        Some(f) => bail!(
            "manifest entry {} points to missing file {}",
            f.id,
            f.path.display()
        ),
        None => bail!("no previous download of `{source}` in manifest; run without --offline"),
    }
}

/// Offline mode for multi-file sources: every manifest entry for the source whose file
/// still exists, oldest first.
pub fn all_from_manifest(paths: &Paths, source: &str) -> Result<Vec<RawFile>> {
    let manifest = Manifest::load(&paths.manifest)?;
    let mut files: Vec<RawFile> = manifest
        .files
        .iter()
        .filter(|f| f.source == source && paths.root.join(&f.path).exists())
        .cloned()
        .collect();
    files.sort_by_key(|f| f.downloaded_at);
    if files.is_empty() {
        bail!("no previous download of `{source}` in manifest; run without --offline");
    }
    Ok(files)
}

/// Resolve a CMS Provider Data Catalog dataset to its current CSV download URL.
pub async fn cms_pdc_distribution(
    client: &reqwest::Client,
    dataset_id: &str,
) -> Result<(String, Option<String>, String)> {
    let url = ehr_atlas_core::sources::CMS_PDC_METASTORE.replace("{id}", dataset_id);
    let meta: serde_json::Value = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let title = meta["title"].as_str().unwrap_or("").to_string();
    let modified = meta["modified"].as_str().map(str::to_string);
    let dist = meta["distribution"]
        .as_array()
        .and_then(|d| d.first())
        .context("dataset has no distribution")?;
    // The metastore nests the real fields under `data` when reference ids are shown,
    // and flattens them otherwise; accept both.
    let download = dist["downloadURL"]
        .as_str()
        .or_else(|| dist["data"]["downloadURL"].as_str())
        .context("distribution has no downloadURL")?;
    Ok((download.to_string(), modified, title))
}

/// One CSV release of a data.cms.gov dataset.
#[derive(Debug, Clone)]
pub struct CmsGovDistribution {
    pub title: String,
    /// `YYYY-MM-DD/YYYY-MM-DD` coverage from the catalog (used to order releases).
    pub temporal: String,
    pub download_url: String,
    pub modified: Option<String>,
}

/// List the CSV distributions of a data.cms.gov dataset (by UUID), newest first.
/// Cost reports and utilization files are one release per year; enrollments and POS are
/// one per month/quarter — the caller decides how many to take.
pub async fn data_cms_gov_distributions(
    client: &reqwest::Client,
    dataset_uuid: &str,
) -> Result<Vec<CmsGovDistribution>> {
    let catalog: serde_json::Value = client
        .get(ehr_atlas_core::sources::DATA_CMS_GOV_CATALOG)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("parsing data.cms.gov data.json")?;
    let needle = format!("/dataset/{dataset_uuid}/");
    let ds = catalog["dataset"]
        .as_array()
        .context("data.json has no dataset array")?
        .iter()
        .find(|d| d["identifier"].as_str().is_some_and(|s| s.contains(&needle)))
        .with_context(|| format!("dataset {dataset_uuid} not in data.cms.gov catalog"))?;
    let modified = ds["modified"].as_str().map(str::to_string);
    let mut out: Vec<CmsGovDistribution> = ds["distribution"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|d| d["format"].as_str() == Some("CSV") || d["mediaType"].as_str() == Some("text/csv"))
        .filter_map(|d| {
            Some(CmsGovDistribution {
                title: d["title"].as_str().unwrap_or("").to_string(),
                temporal: d["temporal"].as_str().unwrap_or("").to_string(),
                download_url: d["downloadURL"].as_str()?.to_string(),
                modified: modified.clone(),
            })
        })
        .collect();
    out.sort_by(|a, b| b.temporal.cmp(&a.temporal).then(b.title.cmp(&a.title)));
    if out.is_empty() {
        bail!("dataset {dataset_uuid} has no CSV distribution");
    }
    Ok(out)
}

/// One entry of a GitHub directory listing.
#[derive(Debug, Clone)]
pub struct GithubEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
}

/// List a directory in a public GitHub repo via the contents API (unauthenticated: 60 req/h).
pub async fn github_dir(client: &reqwest::Client, repo: &str, path: &str) -> Result<Vec<GithubEntry>> {
    let url = ehr_atlas_core::sources::GITHUB_CONTENTS_API
        .replace("{repo}", repo)
        .replace("{path}", path);
    let v: serde_json::Value = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await?;
    let arr = v
        .as_array()
        .with_context(|| format!("{url}: expected a directory listing, got {}", v["message"]))?;
    Ok(arr
        .iter()
        .map(|e| GithubEntry {
            name: e["name"].as_str().unwrap_or("").to_string(),
            path: e["path"].as_str().unwrap_or("").to_string(),
            is_dir: e["type"].as_str() == Some("dir"),
            size: e["size"].as_u64().unwrap_or(0),
        })
        .collect())
}

/// Verify a CSV's header contains every required column; returns the header.
pub fn check_csv_header(path: &std::path::Path, required: &[&str]) -> Result<Vec<String>> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_path(path)?;
    let header: Vec<String> = rdr
        .headers()?
        .iter()
        .map(|h| h.trim_start_matches('\u{feff}').trim().to_string())
        .collect();
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|c| !header.iter().any(|h| h.eq_ignore_ascii_case(c)))
        .collect();
    if !missing.is_empty() {
        bail!(
            "{} is missing expected columns {:?}; upstream schema may have changed. Header was: {:?}",
            path.display(),
            missing,
            header
        );
    }
    Ok(header)
}

/// DuckDB refuses (or, with `ignore_errors`, silently and nondeterministically drops rows
/// of) CSVs that are not valid UTF-8. Returns `path` when it is valid UTF-8; otherwise
/// writes a UTF-8 copy to `data/cache/<source>/` and returns that. Lines that are not valid
/// UTF-8 are decoded as Windows-1252, which is what the federal files that need this use
/// (® ™ – ’ é, 0xA0). Streams line by line, so large files never sit in memory.
pub fn utf8_path(paths: &Paths, source: &str, path: &Path) -> Result<PathBuf> {
    use std::io::{BufRead, BufReader, BufWriter};
    let open = || -> Result<BufReader<std::fs::File>> {
        Ok(BufReader::new(std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?))
    };
    let mut line = Vec::new();
    let mut rdr = open()?;
    let valid = loop {
        line.clear();
        if rdr.read_until(b'\n', &mut line)? == 0 {
            break true;
        }
        if std::str::from_utf8(&line).is_err() {
            break false;
        }
    };
    if valid {
        return Ok(path.to_path_buf());
    }
    let dir = paths.cache.join(source);
    std::fs::create_dir_all(&dir)?;
    let out = dir.join(path.file_name().unwrap_or_default());
    let mut w = BufWriter::new(std::fs::File::create(&out)?);
    let mut rdr = open()?;
    let mut n_decoded = 0usize;
    loop {
        line.clear();
        if rdr.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        match std::str::from_utf8(&line) {
            Ok(s) => w.write_all(s.as_bytes())?,
            Err(_) => {
                n_decoded += 1;
                w.write_all(decode_windows_1252(&line).as_bytes())?;
            }
        }
    }
    w.flush()?;
    tracing::warn!(file = %path.display(), lines_decoded_as_windows_1252 = n_decoded, "not valid UTF-8; loading a converted copy from cache");
    Ok(out)
}

/// Windows-1252 → UTF-8. 0x80–0x9F are the code page's printable extras; the five bytes it
/// leaves undefined become U+FFFD. Everything else maps to the same code point (Latin-1).
fn decode_windows_1252(bytes: &[u8]) -> String {
    const HIGH: [char; 32] = [
        '€', '\u{FFFD}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{FFFD}', 'Ž', '\u{FFFD}',
        '\u{FFFD}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{FFFD}', 'ž', 'Ÿ',
    ];
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9F => HIGH[(b - 0x80) as usize],
            _ => b as char,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::decode_windows_1252;

    #[test]
    fn decodes_windows_1252_extras_and_latin1() {
        assert_eq!(decode_windows_1252(b"RelayClinical\x99 Solutions"), "RelayClinical™ Solutions");
        assert_eq!(decode_windows_1252(b"PatientTouch\xae \x96 Caf\xe9\xa0x"), "PatientTouch® – Café\u{a0}x");
        assert_eq!(decode_windows_1252(b"\x81"), "\u{FFFD}");
    }
}
