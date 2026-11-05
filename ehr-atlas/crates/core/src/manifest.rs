//! Raw-file provenance manifest.
//!
//! Every file the pipeline downloads gets one [`RawFile`] entry recording where it
//! came from, when, and its SHA-256. The manifest lives at `data/raw/manifest.json`
//! and is also loaded into DuckDB as the `source_files` table so every derived row
//! can point back to a `source_file_id`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RawFile {
    /// Stable identifier: `<source_id>:<sha256 prefix>` (e.g. `onc_linkage:3f9a2c1e`).
    pub id: String,
    /// Logical source key, see [`crate::sources::SourceId`].
    pub source: String,
    /// URL actually fetched.
    pub url: String,
    /// Path relative to the repo root, e.g. `data/raw/onc_linkage/2026-09-26_hospital-promoting-interoperability-chpl-linkage.csv`.
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub downloaded_at: DateTime<Utc>,
    /// Upstream "last modified"/"release" date if the catalog exposed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_modified: Option<String>,
    /// Free-form notes (e.g. catalog dataset id, distribution version).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub files: Vec<RawFile>,
}

impl Manifest {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Insert or replace by `id` (same source + same hash ⇒ same id ⇒ idempotent).
    pub fn upsert(&mut self, file: RawFile) {
        if let Some(existing) = self.files.iter_mut().find(|f| f.id == file.id) {
            *existing = file;
        } else {
            self.files.push(file);
        }
    }

    /// Most recently downloaded file for a source.
    pub fn latest(&self, source: &str) -> Option<&RawFile> {
        self.files
            .iter()
            .filter(|f| f.source == source)
            .max_by_key(|f| f.downloaded_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rf(id: &str, source: &str, secs: i64) -> RawFile {
        RawFile {
            id: id.into(),
            source: source.into(),
            url: "u".into(),
            path: PathBuf::from("p"),
            sha256: "s".into(),
            bytes: 1,
            downloaded_at: DateTime::from_timestamp(secs, 0).unwrap(),
            upstream_modified: None,
            notes: None,
        }
    }

    #[test]
    fn upsert_is_idempotent_and_latest_picks_newest() {
        let mut m = Manifest::default();
        m.upsert(rf("a:1", "a", 10));
        m.upsert(rf("a:1", "a", 10));
        m.upsert(rf("a:2", "a", 20));
        m.upsert(rf("b:1", "b", 30));
        assert_eq!(m.files.len(), 3);
        assert_eq!(m.latest("a").unwrap().id, "a:2");
        assert!(m.latest("zzz").is_none());
    }

    #[test]
    fn round_trips_json() {
        let mut m = Manifest::default();
        m.upsert(rf("a:1", "a", 10));
        let p = std::env::temp_dir().join(format!("manifest_{}.json", std::process::id()));
        m.save(&p).unwrap();
        let back = Manifest::load(&p).unwrap();
        assert_eq!(back.files, m.files);
        std::fs::remove_file(p).ok();
    }
}
