//! One module per upstream source.
//!
//! # Contract for enrichment sources
//!
//! Every enrichment module (everything except the four core sources) exposes:
//!
//! ```ignore
//! pub const SOURCE: &str;                                   // manifest source key (SourceId::as_str)
//! pub async fn fetch(paths, client, offline) -> Result<Vec<RawFile>>;  // download or reuse raw files
//! pub fn normalize(conn, paths, files) -> Result<()>;      // raw files → stg_* (DELETE + INSERT)
//! pub fn derive(conn) -> Result<()>;                       // stg_* (+ core tables) → final tables
//! ```
//!
//! * `fetch` must be idempotent: unchanged upstream bytes reuse the existing manifest
//!   entry (`download` handles this) and `--offline` reuses manifest entries.
//! * `normalize` loads with DuckDB `read_csv(..., all_varchar=true)` and casts
//!   explicitly; it validates the header with `check_csv_header` first so upstream
//!   renames fail loudly. Multi-file sources load every file and de-duplicate on the
//!   logical key, keeping the newest release.
//! * `derive` is called by `build` after the core tables exist and only when the
//!   staging table has rows (see `schema::ENRICH_TABLES`); it must `DELETE` and refill
//!   its final table(s) and assert its primary keys hold.
//! * Log row counts with `tracing::info!` at the end of `normalize` and `derive`.

pub mod chpl;
pub mod cms_hospitals;
pub mod cms_pi;
pub mod onc_linkage;

// enrichment, tier 1
pub mod cms_enrollments;
pub mod cms_inpatient;
pub mod cms_measures;
pub mod cms_pos;
pub mod cms_service_area;
pub mod geocode;
pub mod hcris;

// enrichment, tier 2
pub mod ahrq_compendium;
pub mod chpl_products;
pub mod clinicians;
pub mod cms_dac;
pub mod lantern;

// history
pub mod mu_attestation;
