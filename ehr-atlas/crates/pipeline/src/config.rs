//! Filesystem layout and shared run context.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Paths {
    pub root: PathBuf,
    pub raw: PathBuf,
    pub processed: PathBuf,
    pub cache: PathBuf,
    pub config: PathBuf,
    pub reports: PathBuf,
    pub web: PathBuf,
    pub manifest: PathBuf,
    pub db: PathBuf,
}

impl Paths {
    /// Resolve relative to `root` (defaults to the directory containing `Cargo.toml`
    /// found by walking up from CWD, falling back to CWD).
    pub fn new(root: Option<PathBuf>) -> Result<Self> {
        let root = match root {
            Some(r) => r,
            None => find_repo_root().unwrap_or(std::env::current_dir()?),
        };
        let root = root
            .canonicalize()
            .with_context(|| format!("root {}", root.display()))?;
        let data = root.join("data");
        Ok(Self {
            raw: data.join("raw"),
            processed: data.join("processed"),
            cache: data.join("cache"),
            config: root.join("config"),
            reports: root.join("reports"),
            web: root.join("web"),
            manifest: data.join("raw").join("manifest.json"),
            db: data.join("processed").join("ehr_atlas.duckdb"),
            root,
        })
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        for d in [&self.raw, &self.processed, &self.cache, &self.reports] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }

    /// Path relative to the repo root, for storage in the manifest.
    pub fn relative(&self, p: &Path) -> PathBuf {
        p.strip_prefix(&self.root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

fn find_repo_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("Cargo.toml").exists() && dir.join("crates").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}
