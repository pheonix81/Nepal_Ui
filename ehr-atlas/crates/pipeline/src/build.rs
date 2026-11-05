//! Build the final tables from staging.
//!
//! Primary-vendor rule (per hospital-year), applied to canonical vendors after
//! `ref_vendor_map`; see README "Primary vendor rule":
//!   1. a vendor with at least one *core* product (ref_core_products) beats one without;
//!   2. then vendor tier: core_ehr > unmapped > ancillary;
//!   3. then number of certified modules attributed to that vendor;
//!   4. then vendor name (alphabetical) — flagged `is_tie = TRUE`.

use crate::config::Paths;
use crate::db::{count, lit, load_reference_tables, sync_manifest};
use anyhow::{ensure, Result};
use duckdb::Connection;
use ehr_atlas_core::schema;
use tracing::info;

pub fn build(conn: &Connection, paths: &Paths) -> Result<()> {
    sync_manifest(conn, paths)?;
    load_reference_tables(conn, paths)?;
    for t in schema::REBUILT_TABLES {
        conn.execute_batch(&format!("DELETE FROM {t}"))?;
    }
    ensure!(
        count(conn, "SELECT count(*) FROM stg_cms_hospitals")? > 0,
        "stg_cms_hospitals is empty; run `cms-hospitals` first"
    );
    ensure!(
        count(conn, "SELECT count(*) FROM stg_onc_linkage")? > 0,
        "stg_onc_linkage is empty; run `onc-linkage` first"
    );

    conn.execute_batch(HOSPITALS_SQL)?;
    conn.execute_batch(HOSPITAL_EHR_SQL)?;
    conn.execute_batch(&vendor_year_sql("hospital_ehr", "hospital_vendor_year"))?;
    derive_enrichment(conn)?;
    for v in schema::VIEW_DDL {
        conn.execute_batch(v)?;
    }

    // Integrity assertions.
    let dup_ehr = count(
        conn,
        "SELECT count(*) FROM (SELECT ccn, year, cehrt_id, coalesce(chpl_id,'') k, count(*) c
                               FROM hospital_ehr GROUP BY 1,2,3,4 HAVING c > 1)",
    )?;
    ensure!(
        dup_ehr == 0,
        "hospital_ehr has {dup_ehr} duplicate (ccn, year, cehrt_id, chpl_id) keys"
    );
    let orphan = count(conn, "SELECT count(*) FROM hospital_ehr e WHERE NOT EXISTS (SELECT 1 FROM hospitals h WHERE h.ccn = e.ccn)")?;
    ensure!(
        orphan == 0,
        "hospital_ehr has {orphan} rows whose CCN is not in hospitals"
    );

    let bad_override = count(conn, "SELECT count(*) FROM ref_vendor_overrides o WHERE NOT EXISTS (SELECT 1 FROM hospitals h WHERE h.ccn = o.ccn)")?;
    ensure!(
        bad_override == 0,
        "config/vendor_overrides.csv has {bad_override} CCNs that are not in hospitals"
    );

    let n_h = count(conn, "SELECT count(*) FROM hospitals")?;
    let n_e = count(conn, "SELECT count(*) FROM hospital_ehr")?;
    let n_v = count(conn, "SELECT count(*) FROM hospital_vendor_year")?;
    info!(
        hospitals = n_h,
        hospital_ehr = n_e,
        hospital_vendor_year = n_v,
        "final tables built"
    );

    export_parquet(conn, paths)?;
    Ok(())
}

/// Derive every enrichment final table whose staging table has rows. Each source module
/// owns its own `derive`; a source that was never loaded is skipped with a warning so a
/// core-only build still succeeds.
fn derive_enrichment(conn: &Connection) -> Result<()> {
    use crate::sources as s;
    let loaded = |stg: &str| -> Result<bool> { Ok(count(conn, &format!("SELECT count(*) FROM {stg}"))? > 0) };
    let steps: Vec<(&str, &str, fn(&Connection) -> Result<()>)> = vec![
        ("cms_enrollments", "stg_cms_enrollments", s::cms_enrollments::derive),
        ("geocode", "stg_census_geocode", s::geocode::derive),
        ("hcris", "stg_hcris", s::hcris::derive),
        ("cms_pos", "stg_cms_pos", s::cms_pos::derive),
        ("cms_measures", "stg_cms_measures", s::cms_measures::derive),
        ("cms_inpatient", "stg_cms_inpatient", s::cms_inpatient::derive),
        ("cms_service_area", "stg_cms_service_area", s::cms_service_area::derive),
        ("ahrq_compendium", "stg_ahrq_linkage", s::ahrq_compendium::derive),
        ("clinicians", "stg_onc_clinician_cehrt", s::clinicians::derive),
        // after clinicians (fills practice_vendor.practice_id) and geocode (reads stg_census_geocode)
        ("cms_dac", "stg_cms_dac", s::cms_dac::derive),
        ("chpl_products", "stg_chpl_products", s::chpl_products::derive),
        // lantern last: it matches against hospital_npi / hospital_enrollment
        ("lantern", "stg_lantern_endpoints", s::lantern::derive),
        ("mu_attestation", "stg_mu_attestation", s::mu_attestation::derive),
    ];
    for (name, stg, f) in steps {
        if loaded(stg)? {
            f(conn)?;
            info!(source = name, "derived");
        } else {
            tracing::warn!(source = name, staging = stg, "staging empty; skipped (run `ehr-atlas {}` first)", name.replace('_', "-"));
        }
    }
    Ok(())
}

fn export_parquet(conn: &Connection, paths: &Paths) -> Result<()> {
    let mut tables: Vec<&str> = vec![
        "hospitals",
        "hospital_ehr",
        "hospital_vendor_year",
        "hospital_current_vendor",
        "hospital_profile",
        "source_files",
    ];
    for (t, stg) in schema::ENRICH_TABLES {
        if count(conn, &format!("SELECT count(*) FROM {stg}"))? > 0 && !tables.contains(t) {
            tables.push(t);
        }
    }
    for t in tables {
        let out = paths.processed.join(format!("{t}.parquet"));
        conn.execute_batch(&format!(
            "COPY (SELECT * FROM {t}) TO {} (FORMAT PARQUET)",
            lit(&out.to_string_lossy())
        ))?;
    }
    let csv = paths.processed.join("hospital_current_vendor.csv");
    conn.execute_batch(&format!(
        "COPY (SELECT * FROM hospital_current_vendor ORDER BY state, name) TO {} (FORMAT CSV, HEADER)",
        lit(&csv.to_string_lossy())
    ))?;
    info!(dir = %paths.processed.display(), "parquet + csv exports written");
    Ok(())
}

const HOSPITALS_SQL: &str = r#"
INSERT INTO hospitals
SELECT ccn, name, address, city, state, zip, county, phone, hospital_type, ownership,
       emergency_services, birthing_friendly, overall_rating, TRUE, source_file_id
FROM stg_cms_hospitals;

-- CCNs that only appear in PI/ONC files (closed, merged, or renumbered since the HGI snapshot).
INSERT INTO hospitals
SELECT ccn, facility_name, address, city, state, zip, county, phone,
       NULL, NULL, NULL, NULL, NULL, FALSE, source_file_id
FROM (
    SELECT ccn, facility_name, address, city, state, zip, county, phone, year, source_file_id,
           row_number() OVER (PARTITION BY ccn ORDER BY year DESC, src) AS rn
    FROM (
        SELECT ccn, facility_name, address, city, state, zip, county, phone, year, source_file_id, 1 AS src FROM stg_onc_linkage
        UNION ALL
        SELECT ccn, facility_name, address, city, state, zip, county, phone, year, source_file_id, 2 AS src FROM stg_cms_pi
    )
)
WHERE rn = 1 AND facility_name IS NOT NULL
  AND ccn NOT IN (SELECT ccn FROM hospitals);
"#;

const HOSPITAL_EHR_SQL: &str = r#"
-- 1. Attestations: which CEHRT ID did each hospital report for each year, and who says so.
CREATE OR REPLACE TEMP TABLE _attest AS
SELECT ccn, year, cehrt_id, 'onc_linkage' AS attest_source, min(source_file_id) AS source_file_id
FROM stg_onc_linkage
WHERE cehrt_id IS NOT NULL
GROUP BY 1, 2, 3
UNION ALL
SELECT p.ccn, p.year, p.cehrt_id, 'cms_pi', p.source_file_id
FROM stg_cms_pi p
WHERE p.cehrt_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM stg_onc_linkage o
                  WHERE o.ccn = p.ccn AND o.year = p.year AND o.cehrt_id = p.cehrt_id);

-- 2. Resolution: CEHRT ID → certified products. ONC's linkage wins; CHPL fills the rest.
-- CEHRT IDs ONC expanded completely (every product row has a developer).
CREATE OR REPLACE TEMP TABLE _onc_resolved AS
SELECT cehrt_id FROM stg_onc_linkage
WHERE cehrt_id IS NOT NULL
GROUP BY 1
HAVING count(*) FILTER (WHERE developer_name_raw IS NULL) = 0;

CREATE OR REPLACE TEMP TABLE _resolve AS
-- (a) fully resolved by ONC
SELECT DISTINCT cehrt_id, chpl_id, chpl_product_db_id, developer_name_raw, product_name_raw,
       'onc_linkage' AS resolve_source
FROM stg_onc_linkage
WHERE cehrt_id IN (SELECT cehrt_id FROM _onc_resolved)
UNION ALL
-- (b) everything else that CHPL resolved
SELECT DISTINCT c.cehrt_id, c.chpl_id, c.chpl_product_db_id, c.developer_name_raw, c.product_name_raw, 'chpl'
FROM stg_chpl_cehrt c
WHERE c.status = 'ok' AND c.cehrt_id NOT IN (SELECT cehrt_id FROM _onc_resolved)
UNION ALL
-- (c) partially resolved by ONC (some products lack a developer) and not yet in the CHPL cache:
--     keep ONC's rows as-is, developer NULL where ONC left it blank.
SELECT DISTINCT o.cehrt_id, o.chpl_id, o.chpl_product_db_id, o.developer_name_raw, o.product_name_raw, 'onc_linkage'
FROM stg_onc_linkage o
WHERE o.cehrt_id IS NOT NULL
  AND o.cehrt_id NOT IN (SELECT cehrt_id FROM _onc_resolved)
  AND o.cehrt_id NOT IN (SELECT cehrt_id FROM stg_chpl_cehrt WHERE status = 'ok');

-- 3. Join, canonicalise vendor, flag core products.
INSERT INTO hospital_ehr
SELECT a.ccn, a.year, a.cehrt_id, r.chpl_id, r.chpl_product_db_id, r.developer_name_raw, r.product_name_raw,
       CASE WHEN r.developer_name_raw IS NULL THEN NULL ELSE coalesce(m.vendor, r.developer_name_raw) END AS vendor,
       CASE WHEN r.developer_name_raw IS NULL THEN NULL ELSE coalesce(m.vendor_tier, 'unmapped') END AS vendor_tier,
       EXISTS (SELECT 1 FROM ref_core_products p
               WHERE p.vendor = coalesce(m.vendor, r.developer_name_raw)
                 AND r.product_name_raw ILIKE p.product_pattern) AS is_core_product,
       a.attest_source, r.resolve_source, a.source_file_id
FROM _attest a
LEFT JOIN _resolve r USING (cehrt_id)
LEFT JOIN ref_vendor_map m ON lower(trim(m.developer_name_raw)) = lower(trim(r.developer_name_raw));

DROP TABLE _attest; DROP TABLE _onc_resolved; DROP TABLE _resolve;
"#;

/// The primary-vendor rule, from a product-level table (`hospital_ehr` or `hospital_ehr_mu`:
/// ccn, year, cehrt_id, vendor, vendor_tier, is_core_product) into a table shaped like
/// `hospital_vendor_year`.
pub(crate) fn vendor_year_sql(src: &str, dst: &str) -> String {
    VENDOR_YEAR_SQL.replace("{src}", src).replace("{dst}", dst)
}

const VENDOR_YEAR_SQL: &str = r#"
CREATE OR REPLACE TEMP TABLE _vend AS
SELECT ccn, year, vendor,
       count(*)                                        AS n_modules,
       bool_or(is_core_product)                        AS has_core,
       max(CASE vendor_tier WHEN 'core_ehr' THEN 2 WHEN 'unmapped' THEN 1 ELSE 0 END) AS tier_rank,
       arg_max(cehrt_id, is_core_product)              AS cehrt_id
FROM {src}
WHERE vendor IS NOT NULL
GROUP BY 1, 2, 3;

CREATE OR REPLACE TEMP TABLE _ranked AS
SELECT *, row_number() OVER (PARTITION BY ccn, year
                             ORDER BY has_core DESC, tier_rank DESC, n_modules DESC, vendor ASC) AS rn
FROM _vend;

INSERT INTO {dst}
SELECT hy.ccn, hy.year,
       r1.vendor, r1.cehrt_id,
       hy.n_modules, hy.n_vendors, hy.n_core_vendors, hy.core_vendors, hy.all_vendors,
       CASE WHEN r1.vendor IS NULL                       THEN 'unresolved'
            WHEN r2.vendor IS NULL                       THEN 'single_vendor'
            WHEN r1.has_core AND NOT r2.has_core         THEN 'core_product'
            WHEN r1.tier_rank > r2.tier_rank             THEN 'vendor_tier'
            WHEN r1.n_modules > r2.n_modules             THEN 'module_count'
            ELSE 'alphabetical_tie' END                  AS rule,
       coalesce(r2.vendor IS NOT NULL AND r1.has_core = r2.has_core
                AND r1.tier_rank = r2.tier_rank AND r1.n_modules = r2.n_modules, FALSE) AS is_tie
FROM (
    SELECT ccn, year,
           count(*)                                                        AS n_modules,
           count(DISTINCT vendor)                                          AS n_vendors,
           count(DISTINCT vendor) FILTER (WHERE vendor_tier = 'core_ehr')  AS n_core_vendors,
           array_to_string(list_sort(list_distinct(list(vendor) FILTER (WHERE vendor_tier = 'core_ehr'))), '|') AS core_vendors,
           array_to_string(list_sort(list_distinct(list(vendor))), '|')    AS all_vendors
    FROM {src}
    GROUP BY 1, 2
) hy
LEFT JOIN _ranked r1 ON r1.ccn = hy.ccn AND r1.year = hy.year AND r1.rn = 1
LEFT JOIN _ranked r2 ON r2.ccn = hy.ccn AND r2.year = hy.year AND r2.rn = 2;

DROP TABLE _vend; DROP TABLE _ranked;
"#;
