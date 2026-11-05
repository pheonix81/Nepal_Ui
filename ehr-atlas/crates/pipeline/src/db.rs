//! DuckDB connection helpers.

use crate::config::Paths;
use anyhow::{Context, Result};
use duckdb::{params, Connection};
use ehr_atlas_core::manifest::Manifest;
use ehr_atlas_core::schema;
use tracing::debug;

pub fn open(paths: &Paths) -> Result<Connection> {
    std::fs::create_dir_all(&paths.processed)?;
    let conn =
        Connection::open(&paths.db).with_context(|| format!("opening {}", paths.db.display()))?;
    drop_stale_placeholders(&conn)?;
    for ddl in schema::DDL.iter().chain(schema::ENRICH_DDL) {
        conn.execute_batch(ddl)?;
    }
    Ok(conn)
}

/// Earlier builds created empty placeholder tables for later phases with narrower
/// column sets. `CREATE TABLE IF NOT EXISTS` would keep those, so drop any table that
/// lacks a column the current schema requires. Everything in `data/processed` is
/// regenerable, so this is safe.
fn drop_stale_placeholders(conn: &Connection) -> Result<()> {
    const SENTINELS: &[(&str, &str)] = &[
        ("hospital_npi", "dba_name"),
        ("hospital_cost_reports", "bed_days_available"),
        ("fhir_endpoints", "match_score"),
        ("clinician_cehrt", "vendor_tier"),
        ("ref_measures", "mode"),
        ("hospital_systems", "corp_parent_id"),
        ("health_systems", "aha_sysid"),
        ("stg_ahrq_linkage", "compendium_hospital_id"),
        ("stg_ahrq_systems", "aha_sysid"),
        ("practice_vendor", "practice_id"),
        ("practices", "lantern_vendor"),
        ("fhir_endpoints", "practice_id"),
    ];
    for (table, col) in SENTINELS {
        let exists = count(
            conn,
            &format!("SELECT count(*) FROM information_schema.tables WHERE table_name = {}", lit(table)),
        )? > 0;
        if !exists {
            continue;
        }
        let has_col = count(
            conn,
            &format!(
                "SELECT count(*) FROM information_schema.columns WHERE table_name = {} AND column_name = {}",
                lit(table),
                lit(col)
            ),
        )? > 0;
        if !has_col {
            tracing::warn!(table, "dropping stale placeholder table (schema changed)");
            conn.execute_batch(&format!("DROP TABLE IF EXISTS {table}"))?;
        }
    }
    Ok(())
}

/// SQL-escape a string literal.
pub fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

pub fn count(conn: &Connection, sql: &str) -> Result<i64> {
    Ok(conn.query_row(sql, [], |r| r.get::<_, i64>(0))?)
}

/// Replace `source_files` with the current manifest.
pub fn sync_manifest(conn: &Connection, paths: &Paths) -> Result<()> {
    let manifest = Manifest::load(&paths.manifest)?;
    conn.execute_batch("DELETE FROM source_files")?;
    let mut stmt = conn.prepare(
        "INSERT INTO source_files (id, source, url, path, sha256, bytes, downloaded_at, upstream_modified, notes)
         VALUES (?, ?, ?, ?, ?, ?, CAST(? AS TIMESTAMP), ?, ?)",
    )?;
    for f in &manifest.files {
        stmt.execute(params![
            f.id,
            f.source,
            f.url,
            f.path.to_string_lossy().to_string(),
            f.sha256,
            f.bytes as i64,
            f.downloaded_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            f.upstream_modified,
            f.notes,
        ])?;
    }
    debug!(
        n = manifest.files.len(),
        "synced manifest into source_files"
    );
    Ok(())
}

/// Load the reviewable reference CSVs from `config/`.
pub fn load_reference_tables(conn: &Connection, paths: &Paths) -> Result<()> {
    let vm = paths.config.join("vendor_map.csv");
    let cp = paths.config.join("core_products.csv");
    let sc = paths.config.join("spot_checks.csv");
    let ov = paths.config.join("vendor_overrides.csv");
    // Validate the vendor map through the typed loader first (dupes, tier values).
    ehr_atlas_core::vendor::VendorMap::load(&vm)?;
    conn.execute_batch(&format!(
        r#"
        DELETE FROM ref_vendor_map;
        INSERT INTO ref_vendor_map
            SELECT trim(developer_name_raw), trim(vendor), trim(vendor_tier), notes
            FROM read_csv({vm}, header=true, all_varchar=true);
        DELETE FROM ref_core_products;
        INSERT INTO ref_core_products
            SELECT trim(vendor), trim(product_pattern), notes
            FROM read_csv({cp}, header=true, all_varchar=true);
        DELETE FROM ref_spot_checks;
        INSERT INTO ref_spot_checks
            SELECT trim(ccn), trim(name), trim(expected_vendor), notes
            FROM read_csv({sc}, header=true, all_varchar=true);
        DELETE FROM ref_vendor_overrides;
        INSERT INTO ref_vendor_overrides
            SELECT trim(ccn), trim(name), trim(vendor), CAST(trim(as_of) AS DATE), trim(source), notes
            FROM read_csv({ov}, header=true, all_varchar=true);
        "#,
        vm = lit(&vm.to_string_lossy()),
        cp = lit(&cp.to_string_lossy()),
        sc = lit(&sc.to_string_lossy()),
        ov = lit(&ov.to_string_lossy()),
    ))?;
    Ok(())
}
