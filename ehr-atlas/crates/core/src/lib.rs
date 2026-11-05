//! Shared types and schema for EHR Atlas.
//!
//! This crate is deliberately free of I/O-heavy dependencies (no DuckDB, no HTTP)
//! so that a future `crates/api` can depend on it for row types and the canonical
//! schema without pulling in the pipeline's build-time cost.

pub mod manifest;
pub mod schema;
pub mod sources;
pub mod vendor;
