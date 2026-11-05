//! Vendor canonicalisation, driven by `config/vendor_map.csv`.
//!
//! The CSV is the source of truth; this module only loads and validates it and offers
//! an in-memory lookup for code paths that need it outside DuckDB (e.g. the CHPL client
//! reporting unmapped developers).

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VendorTier {
    /// Sells an inpatient EHR that can plausibly be a hospital's system of record.
    CoreEhr,
    /// Ancillary/bolt-on modules only (public-health reporting, portals, eCQM, HIE, ...).
    Ancillary,
}

impl VendorTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            VendorTier::CoreEhr => "core_ehr",
            VendorTier::Ancillary => "ancillary",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorMapRow {
    pub developer_name_raw: String,
    pub vendor: String,
    pub vendor_tier: VendorTier,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Default)]
pub struct VendorMap {
    rows: Vec<VendorMapRow>,
    by_raw: HashMap<String, usize>,
}

impl VendorMap {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut rdr = csv::Reader::from_path(path)
            .with_context(|| format!("reading vendor map {}", path.display()))?;
        let mut map = VendorMap::default();
        for rec in rdr.deserialize::<VendorMapRow>() {
            let row = rec?;
            let key = normalize_key(&row.developer_name_raw);
            if map.by_raw.contains_key(&key) {
                bail!(
                    "duplicate developer_name_raw in vendor map: {}",
                    row.developer_name_raw
                );
            }
            map.by_raw.insert(key, map.rows.len());
            map.rows.push(row);
        }
        Ok(map)
    }

    pub fn lookup(&self, developer_name_raw: &str) -> Option<&VendorMapRow> {
        self.by_raw
            .get(&normalize_key(developer_name_raw))
            .map(|&i| &self.rows[i])
    }

    pub fn rows(&self) -> &[VendorMapRow] {
        &self.rows
    }
}

/// Case/whitespace-insensitive key; the SQL side uses `lower(trim(x))` to match.
pub fn normalize_key(s: &str) -> String {
    s.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(tag: &str, content: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("vendor_map_{}_{tag}.csv", std::process::id()));
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn loads_and_looks_up_case_insensitively() {
        let p = write_tmp("lookup", "developer_name_raw,vendor,vendor_tier,notes\nOracle Health,Oracle Health,core_ehr,\n\"Medisolv, Inc.\",Medisolv,ancillary,x\n");
        let m = VendorMap::load(&p).unwrap();
        assert_eq!(
            m.lookup("  oracle health ").unwrap().vendor,
            "Oracle Health"
        );
        assert_eq!(
            m.lookup("Medisolv, Inc.").unwrap().vendor_tier,
            VendorTier::Ancillary
        );
        assert!(m.lookup("Nobody").is_none());
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn rejects_duplicates_and_bad_tiers() {
        let p = write_tmp("dupes", "developer_name_raw,vendor,vendor_tier,notes\nEpic,Epic,core_ehr,\nepic,Epic,core_ehr,\n");
        assert!(VendorMap::load(&p).is_err());
        std::fs::write(
            &p,
            "developer_name_raw,vendor,vendor_tier,notes\nEpic,Epic,primary,\n",
        )
        .unwrap();
        assert!(VendorMap::load(&p).is_err());
        std::fs::remove_file(p).ok();
    }
}
