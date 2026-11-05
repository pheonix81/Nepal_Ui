//! Data-quality report → `reports/data_quality.md`.

use crate::config::Paths;
use crate::db::count;
use anyhow::Result;
use chrono::Utc;
use duckdb::types::{TimeUnit, Value};
use duckdb::Connection;
use std::fmt::Write as _;
use std::path::PathBuf;

fn fmt_value(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Boolean(b) => {
            if *b {
                "yes".into()
            } else {
                "no".into()
            }
        }
        Value::TinyInt(n) => n.to_string(),
        Value::SmallInt(n) => n.to_string(),
        Value::Int(n) => n.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::HugeInt(n) => n.to_string(),
        Value::UTinyInt(n) => n.to_string(),
        Value::USmallInt(n) => n.to_string(),
        Value::UInt(n) => n.to_string(),
        Value::UBigInt(n) => n.to_string(),
        Value::Float(f) => fmt_float(*f as f64),
        Value::Double(f) => fmt_float(*f),
        Value::Text(s) => s.replace('|', "\\|").replace('\n', " "),
        Value::Timestamp(TimeUnit::Microsecond, us) => chrono::DateTime::from_timestamp_micros(*us)
            .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_default(),
        Value::Date32(d) => chrono::DateTime::from_timestamp((*d as i64) * 86_400, 0)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default(),
        other => format!("{other:?}"),
    }
}

/// Up to three decimals, trailing zeros trimmed (so 45.5 stays 45.5 and 0.978 is not shown as 1.0).
fn fmt_float(f: f64) -> String {
    if f.fract() == 0.0 {
        return format!("{f:.0}");
    }
    let t = format!("{f:.3}");
    t.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Run a query and render it as a GitHub-flavoured markdown table.
fn md_table(conn: &Connection, sql: &str) -> Result<String> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([])?;
    let mut out = String::new();
    let mut header_done = false;
    let mut n = 0usize;
    while let Some(row) = rows.next()? {
        if !header_done {
            let names: Vec<String> = row
                .as_ref()
                .column_names()
                .iter()
                .map(|s| s.to_string())
                .collect();
            writeln!(out, "| {} |", names.join(" | "))?;
            writeln!(
                out,
                "|{}|",
                names.iter().map(|_| "---").collect::<Vec<_>>().join("|")
            )?;
            header_done = true;
        }
        let ncol = row.as_ref().column_count();
        let cells: Vec<String> = (0..ncol)
            .map(|i| fmt_value(&row.get::<_, Value>(i).unwrap_or(Value::Null)))
            .collect();
        writeln!(out, "| {} |", cells.join(" | "))?;
        n += 1;
    }
    if n == 0 {
        out.push_str("_none_\n");
    }
    Ok(out)
}

fn scalar(conn: &Connection, sql: &str) -> Result<String> {
    Ok(fmt_value(
        &conn.query_row(sql, [], |r| r.get::<_, Value>(0))?,
    ))
}

pub fn write_report(conn: &Connection, paths: &Paths) -> Result<PathBuf> {
    let mut md = String::new();
    let w = &mut md;

    writeln!(w, "# EHR Atlas — data quality report\n")?;
    writeln!(
        w,
        "Generated {} by `ehr-atlas report`. Regenerate after every build; do not hand-edit.\n",
        Utc::now().format("%Y-%m-%d %H:%M UTC")
    )?;

    writeln!(w, "## Sources\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT source, path, upstream_modified AS "upstream date", downloaded_at, bytes, substr(sha256, 1, 12) AS sha256
        FROM source_files ORDER BY source, downloaded_at"#
        )?
    )?;
    writeln!(
        w,
        "CHPL cache: {} CEHRT IDs on disk, {} resolved OK, {} not found / error / no products.\n",
        scalar(conn, "SELECT count(DISTINCT cehrt_id) FROM stg_chpl_cehrt")?,
        scalar(
            conn,
            "SELECT count(DISTINCT cehrt_id) FROM stg_chpl_cehrt WHERE status = 'ok'"
        )?,
        scalar(
            conn,
            "SELECT count(DISTINCT cehrt_id) FROM stg_chpl_cehrt WHERE status <> 'ok'"
        )?
    )?;

    writeln!(w, "## Table sizes\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT 'hospitals' AS "table", count(*) AS rows FROM hospitals
        UNION ALL SELECT 'hospital_ehr', count(*) FROM hospital_ehr
        UNION ALL SELECT 'hospital_vendor_year', count(*) FROM hospital_vendor_year
        UNION ALL SELECT 'hospital_current_vendor (with vendor)', count(*) FROM hospital_current_vendor WHERE vendor IS NOT NULL
        UNION ALL SELECT 'stg_cms_hospitals', count(*) FROM stg_cms_hospitals
        UNION ALL SELECT 'stg_onc_linkage', count(*) FROM stg_onc_linkage
        UNION ALL SELECT 'stg_cms_pi', count(*) FROM stg_cms_pi
        UNION ALL SELECT 'stg_chpl_cehrt', count(*) FROM stg_chpl_cehrt"#
        )?
    )?;

    writeln!(w, "## 1. Match rate: CMS hospitals → vendor, by year\n")?;
    writeln!(w, "Denominator is every CCN in CMS Hospital General Information. Psychiatric, VA, DoD and most children's hospitals are not in the Medicare Promoting Interoperability program, so they are expected to be unmatched; the second table breaks this out.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        WITH years AS (SELECT DISTINCT year FROM hospital_vendor_year),
        base AS (SELECT count(*) AS n FROM hospitals WHERE in_cms_hgi)
        SELECT y.year,
               (SELECT n FROM base) AS "CMS hospitals",
               count(DISTINCT v.ccn) FILTER (WHERE h.in_cms_hgi) AS "with any EHR row",
               count(DISTINCT v.ccn) FILTER (WHERE h.in_cms_hgi AND v.primary_vendor IS NOT NULL) AS "with primary vendor",
               round(100.0 * count(DISTINCT v.ccn) FILTER (WHERE h.in_cms_hgi AND v.primary_vendor IS NOT NULL) / (SELECT n FROM base), 1) AS "match %",
               count(DISTINCT v.ccn) FILTER (WHERE NOT h.in_cms_hgi) AS "CCNs not in CMS list"
        FROM years y
        LEFT JOIN hospital_vendor_year v ON v.year = y.year
        LEFT JOIN hospitals h ON h.ccn = v.ccn
        GROUP BY y.year ORDER BY y.year"#
        )?
    )?;
    writeln!(w, "### By hospital type (latest year)\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT h.hospital_type AS "hospital type", count(*) AS hospitals,
               count(c.vendor) AS "with vendor",
               round(100.0 * count(c.vendor) / count(*), 1) AS "match %"
        FROM hospitals h LEFT JOIN hospital_current_vendor c ON c.ccn = h.ccn
        WHERE h.in_cms_hgi
        GROUP BY 1 ORDER BY hospitals DESC"#
        )?
    )?;
    writeln!(w, "### Acute-care + critical-access hospitals only\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        WITH years AS (SELECT DISTINCT year FROM hospital_vendor_year),
        base AS (SELECT count(*) AS n FROM hospitals WHERE in_cms_hgi AND hospital_type IN ('Acute Care Hospitals','Critical Access Hospitals'))
        SELECT y.year, (SELECT n FROM base) AS "ACH + CAH",
               count(DISTINCT v.ccn) FILTER (WHERE v.primary_vendor IS NOT NULL) AS "with primary vendor",
               round(100.0 * count(DISTINCT v.ccn) FILTER (WHERE v.primary_vendor IS NOT NULL) / (SELECT n FROM base), 1) AS "match %"
        FROM years y
        LEFT JOIN hospital_vendor_year v ON v.year = y.year
        JOIN hospitals h ON h.ccn = v.ccn AND h.in_cms_hgi AND h.hospital_type IN ('Acute Care Hospitals','Critical Access Hospitals')
        GROUP BY y.year ORDER BY y.year"#
        )?
    )?;
    writeln!(w, "Hospitals present in the PI files with no CEHRT ID reported (attested nothing that year): {}\n",
        md_table(conn, r#"SELECT year, count(DISTINCT ccn) AS hospitals FROM stg_onc_linkage WHERE cehrt_id IS NULL GROUP BY 1 ORDER BY 1"#)?)?;

    writeln!(w, "## 2. Unresolved CEHRT IDs\n")?;
    writeln!(w, "CEHRT IDs attested by a hospital that neither ONC's linkage nor the CHPL cache expands to products. Fix by running `ehr-atlas chpl` with `CHPL_API_KEY` set.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT e.cehrt_id, e.attest_source AS source, string_agg(DISTINCT CAST(e.year AS VARCHAR), ',') AS years,
               count(DISTINCT e.ccn) AS hospitals,
               coalesce(c.status, 'not in CHPL cache') AS "CHPL status",
               any_value(e.chpl_id) AS "CHPL id from ONC"
        FROM hospital_ehr e LEFT JOIN (SELECT cehrt_id, any_value(status) AS status FROM stg_chpl_cehrt GROUP BY 1) c USING (cehrt_id)
        WHERE e.resolve_source IS NULL OR e.developer_name_raw IS NULL
        GROUP BY 1, 2, 5 ORDER BY hospitals DESC"#
        )?
    )?;
    writeln!(
        w,
        "Hospital-years affected: {}\n",
        scalar(
            conn,
            "SELECT count(*) FROM hospital_vendor_year WHERE primary_vendor IS NULL"
        )?
    )?;

    writeln!(w, "## 3. Hospitals with multiple vendors\n")?;
    writeln!(w, "A CEHRT ID bundles several certified modules, often from more than one developer (e.g. Epic plus an Oracle Health eCQM module). Only *core-EHR-tier* vendors matter for the primary-vendor question.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT year, count(*) AS "hospital-years",
               count(*) FILTER (WHERE n_vendors > 1) AS ">1 vendor (any tier)",
               count(*) FILTER (WHERE n_core_vendors > 1) AS ">1 core-EHR vendor",
               count(*) FILTER (WHERE is_tie) AS "unbroken ties"
        FROM hospital_vendor_year GROUP BY 1 ORDER BY 1"#
        )?
    )?;
    writeln!(w, "### How the primary vendor was decided\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"SELECT rule, count(*) AS "hospital-years" FROM hospital_vendor_year GROUP BY 1 ORDER BY 2 DESC"#
        )?
    )?;
    writeln!(w, "### Most common core-vendor combinations\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT core_vendors AS "core vendors present", primary_vendor AS "chosen", count(*) AS "hospital-years"
        FROM hospital_vendor_year WHERE n_core_vendors > 1
        GROUP BY 1, 2 ORDER BY 3 DESC LIMIT 15"#
        )?
    )?;
    writeln!(w, "### Vendor changes between years\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT a.primary_vendor AS "from", b.primary_vendor AS "to", a.year AS "year from", b.year AS "year to", count(*) AS hospitals
        FROM hospital_vendor_year a
        JOIN hospital_vendor_year b ON b.ccn = a.ccn AND b.year = a.year + 1
        WHERE a.primary_vendor IS NOT NULL AND b.primary_vendor IS NOT NULL AND a.primary_vendor <> b.primary_vendor
        GROUP BY 1, 2, 3, 4 ORDER BY hospitals DESC LIMIT 20"#
        )?
    )?;
    writeln!(
        w,
        "Hospitals whose primary vendor changed year over year: {}\n",
        scalar(
            conn,
            r#"
        SELECT count(DISTINCT a.ccn) FROM hospital_vendor_year a JOIN hospital_vendor_year b ON b.ccn = a.ccn AND b.year = a.year + 1
        WHERE a.primary_vendor IS NOT NULL AND b.primary_vendor IS NOT NULL AND a.primary_vendor <> b.primary_vendor"#
        )?
    )?;

    writeln!(w, "## 4. Vendor share by hospital count\n")?;
    writeln!(w, "Primary vendor per hospital, latest year available for that hospital (`hospital_current_vendor`).\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT vendor, count(*) AS hospitals, round(100.0 * count(*) / sum(count(*)) OVER (), 1) AS "share %",
               count(*) FILTER (WHERE hospital_type = 'Acute Care Hospitals') AS "acute care",
               count(*) FILTER (WHERE hospital_type = 'Critical Access Hospitals') AS "critical access",
               count(*) FILTER (WHERE hospital_type NOT IN ('Acute Care Hospitals','Critical Access Hospitals') OR hospital_type IS NULL) AS "other/unknown"
        FROM hospital_current_vendor WHERE vendor IS NOT NULL
        GROUP BY 1 ORDER BY hospitals DESC LIMIT 25"#
        )?
    )?;
    writeln!(w, "### By year (all hospital-years, not just latest)\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        PIVOT (SELECT primary_vendor AS vendor, year FROM hospital_vendor_year WHERE primary_vendor IS NOT NULL)
        ON year USING count(*) GROUP BY vendor ORDER BY 2 DESC LIMIT 15"#
        )?
    )?;
    writeln!(w, "### By state (top vendor per state, latest year)\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT state, count(*) AS hospitals,
               arg_max(vendor, n) AS "top vendor", round(100.0 * max(n) / count(*), 0) AS "top vendor %"
        FROM (SELECT state, vendor, count(*) OVER (PARTITION BY state, vendor) AS n FROM hospital_current_vendor WHERE vendor IS NOT NULL)
        GROUP BY state ORDER BY state"#
        )?
    )?;

    writeln!(w, "## 5. Spot checks\n")?;
    writeln!(w, "Well-known hospitals with an externally known vendor (see `config/spot_checks.csv`). `all vendors` shows every developer under the CEHRT ID.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT s.ccn, s.name, s.expected_vendor AS expected,
               v23.primary_vendor AS "2023", v24.primary_vendor AS "2024",
               CASE WHEN c.vendor = s.expected_vendor THEN 'PASS' WHEN c.vendor IS NULL THEN 'NO DATA' ELSE 'FAIL' END AS result,
               c.core_vendors AS "core vendors", c.rule, s.notes
        FROM ref_spot_checks s
        LEFT JOIN hospital_current_vendor c ON c.ccn = s.ccn
        LEFT JOIN hospital_vendor_year v23 ON v23.ccn = s.ccn AND v23.year = 2023
        LEFT JOIN hospital_vendor_year v24 ON v24.ccn = s.ccn AND v24.year = 2024
        ORDER BY s.ccn"#
        )?
    )?;

    writeln!(w, "### Vendor overrides\n")?;
    writeln!(w, "Hand-entered corrections from `config/vendor_overrides.csv`, for hospitals that switched vendor after their latest attestation or are outside the PI program. These win over the attested vendor (`rule = override`).\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT c.ccn, c.name, c.state, c.attested_vendor AS attested, c.attested_year AS year,
               c.vendor AS override, c.override_as_of AS "as of", c.override_source AS source, c.override_notes AS notes
        FROM hospital_current_vendor c WHERE c.rule = 'override'
        ORDER BY c.override_as_of DESC, c.state, c.name"#
        )?
    )?;

    writeln!(w, "## 6. Things that look wrong or need review\n")?;
    writeln!(w, "### Developer names not in `config/vendor_map.csv`\n")?;
    writeln!(w, "These fall through with `vendor = developer_name_raw`, `vendor_tier = unmapped`. Add them to the map.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT developer_name_raw AS developer, count(*) AS modules, count(DISTINCT ccn) AS hospitals, any_value(product_name_raw) AS "example product"
        FROM hospital_ehr WHERE vendor_tier = 'unmapped' GROUP BY 1 ORDER BY 2 DESC"#
        )?
    )?;
    writeln!(w, "### Primary vendor is an ancillary-tier developer\n")?;
    writeln!(w, "No core-EHR vendor appeared under the CEHRT ID; the assignment is a best guess and probably wrong.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT v.year, v.primary_vendor, count(*) AS hospitals, any_value(h.name) AS example
        FROM hospital_vendor_year v JOIN hospitals h USING (ccn)
        WHERE v.rule <> 'unresolved' AND v.n_core_vendors = 0
        GROUP BY 1, 2 ORDER BY 3 DESC LIMIT 20"#
        )?
    )?;
    writeln!(w, "### Unbroken ties\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT v.ccn, h.name, v.year, v.primary_vendor AS chosen, v.core_vendors, v.all_vendors
        FROM hospital_vendor_year v JOIN hospitals h USING (ccn) WHERE v.is_tie ORDER BY v.year, v.ccn LIMIT 30"#
        )?
    )?;
    writeln!(w, "### Core-EHR vendor chosen without a core product row\n")?;
    writeln!(w, "Chosen on tier/module count only: the vendor's core inpatient product was not among the certified modules (common for MEDITECH sites that list only interfaces). Usually right, worth knowing.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT v.year, v.primary_vendor, count(*) AS hospitals
        FROM hospital_vendor_year v
        WHERE v.rule <> 'unresolved'
          AND NOT EXISTS (SELECT 1 FROM hospital_ehr e WHERE e.ccn = v.ccn AND e.year = v.year AND e.vendor = v.primary_vendor AND e.is_core_product)
        GROUP BY 1, 2 ORDER BY 3 DESC LIMIT 15"#
        )?
    )?;
    writeln!(
        w,
        "### CCNs in EHR data but not in CMS Hospital General Information\n"
    )?;
    writeln!(w, "Kept in `hospitals` with `in_cms_hgi = FALSE` (no type/ownership). Typically closed, merged, or renumbered since the CMS snapshot.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT h.ccn, h.name, h.state, string_agg(DISTINCT CAST(v.year AS VARCHAR), ',') AS years, any_value(v.primary_vendor) AS vendor
        FROM hospitals h LEFT JOIN hospital_vendor_year v USING (ccn) WHERE NOT h.in_cms_hgi
        GROUP BY 1, 2, 3 ORDER BY 1 LIMIT 80"#
        )?
    )?;
    writeln!(
        w,
        "Total: {}\n",
        scalar(conn, "SELECT count(*) FROM hospitals WHERE NOT in_cms_hgi")?
    )?;
    writeln!(w, "### CMS PI file vs ONC linkage for the same year\n")?;
    writeln!(w, "Where both cover a year, the CMS Provider Data Catalog release is newer than the snapshot ONC linked; hospitals whose CEHRT ID differs between the two get both IDs in `hospital_ehr` (attest_source tells them apart).\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT p.year,
               count(*) AS "CMS PI attestations",
               count(*) FILTER (WHERE o.ccn IS NOT NULL) AS "same CEHRT ID in ONC",
               count(*) FILTER (WHERE o.ccn IS NULL AND o2.ccn IS NOT NULL) AS "hospital in ONC, different CEHRT ID",
               count(*) FILTER (WHERE o2.ccn IS NULL) AS "hospital not in ONC that year"
        FROM stg_cms_pi p
        LEFT JOIN (SELECT DISTINCT ccn, year, cehrt_id FROM stg_onc_linkage) o ON o.ccn = p.ccn AND o.year = p.year AND o.cehrt_id = p.cehrt_id
        LEFT JOIN (SELECT DISTINCT ccn, year FROM stg_onc_linkage) o2 ON o2.ccn = p.ccn AND o2.year = p.year
        WHERE p.cehrt_id IS NOT NULL
        GROUP BY 1 ORDER BY 1"#
        )?
    )?;
    writeln!(w, "### Hospital-years with two CEHRT IDs\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT year, count(*) AS "hospital-years", any_value(ccn) AS example
        FROM (SELECT ccn, year, count(DISTINCT cehrt_id) AS n FROM hospital_ehr GROUP BY 1, 2 HAVING n > 1)
        GROUP BY 1 ORDER BY 1"#
        )?
    )?;
    writeln!(w, "### Attestation flag\n")?;
    writeln!(w, "Rows where the hospital did *not* meet PI criteria but still reported a CEHRT ID are kept; the flag is in staging only for now.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT year, meets_pi_criteria AS "meets PI criteria", count(DISTINCT ccn) AS hospitals
        FROM stg_onc_linkage WHERE cehrt_id IS NOT NULL GROUP BY 1, 2 ORDER BY 1, 2"#
        )?
    )?;

    writeln!(w, "## 7. Enrichment coverage\n")?;
    writeln!(w, "One row per enrichment table: rows loaded (all CCNs, including units and non-hospital providers), distinct CMS hospitals covered, and that as a share of the CMS hospital list. Empty rows mean the source has not been run (`ehr-atlas enrich-all`).\n")?;
    writeln!(w, "{}", md_table(conn, ENRICH_COVERAGE_SQL)?)?;
    writeln!(w, "### Geocoding\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT g.method, count(*) AS hospitals, round(100.0 * count(*) / (SELECT count(*) FROM hospital_geo), 1) AS "%"
        FROM hospital_geo g GROUP BY 1 ORDER BY 2 DESC"#
        )?
    )?;
    if count(conn, "SELECT count(*) FROM practices")? > 0 {
        writeln!(w, "### Practice identity (MIPS groups → CMS Doctors & Clinicians)\n")?;
        writeln!(w, "Each MIPS group's clinicians are looked up in the Doctors & Clinicians file; the organisation (group PAC ID) listing the most of them (ties broken by a location in the group's practice state) becomes the practice. A one-clinician group whose clinician has no organisation becomes a solo practice. A match must cover 20% of the group's clinicians, or at least three clinicians covering 5%.\n")?;
        writeln!(
            w,
            "{}",
            md_table(
                conn,
                r#"
        SELECT coalesce(match_method, 'unmatched') AS "match", count(*) AS "groups", sum(n_clinicians) AS clinicians,
               round(100.0 * count(*) / sum(count(*)) OVER (), 1) AS "% of groups",
               round(avg(match_share), 2) AS "mean share of clinicians found"
        FROM practice_vendor GROUP BY 1 ORDER BY 2 DESC"#
            )?
        )?;
        writeln!(
            w,
            "{}",
            md_table(
                conn,
                r#"
        SELECT geo_method AS "practice location", count(*) AS practices, round(100.0 * count(*) / sum(count(*)) OVER (), 1) AS "%"
        FROM practices GROUP BY 1 ORDER BY 2 DESC"#
            )?
        )?;
        writeln!(w, "Groups matched with under half of their clinicians found under the organisation (name and address may belong to a related entity):\n")?;
        writeln!(
            w,
            "{}",
            md_table(
                conn,
                r#"
        SELECT v.group_key, v.practice_state AS state, v.n_clinicians AS clinicians, round(v.match_share, 2) AS "share found",
               p.name AS "matched organisation", p.n_clinicians AS "org clinicians", v.vendor
        FROM practice_vendor v JOIN practices p USING (practice_id)
        WHERE v.match_method = 'org' AND v.match_share < 0.5
        ORDER BY v.n_clinicians DESC LIMIT 15"#
            )?
        )?;
    }
    if count(conn, "SELECT count(*) FROM fhir_endpoints WHERE practice_id IS NOT NULL")? > 0 {
        writeln!(w, "### Lantern endpoint organizations → practices (#19)\n")?;
        writeln!(w, "Organizations in core-EHR vendors' FHIR endpoint lists that are not hospitals, collapsed to one organization per NPI (else per name + state + ZIP) and matched to CMS Doctors & Clinicians organisations: by NPI (a clinician's organisation), exact name + ZIP, exact name unique in the state, exact name unique nationally (no state on the list), then Jaro-Winkler within the state against step-2 practices. Unmatched organizations become `practices` rows with `source = lantern` (lower confidence; located by ZIP centroid when the list has a ZIP).\n")?;
        writeln!(
            w,
            "{}",
            md_table(
                conn,
                r#"
        SELECT coalesce(f.practice_match_method, 'no match (Lantern-only practice)') AS "match",
               count(*) AS endpoints, count(DISTINCT f.practice_id) AS practices,
               count(DISTINCT f.practice_id) FILTER (WHERE p.source = 'cms_dac') AS "step-2 practices",
               round(avg(f.practice_match_score), 3) AS "mean score"
        FROM fhir_endpoints f JOIN practices p USING (practice_id)
        GROUP BY 1 ORDER BY 2 DESC"#
            )?
        )?;
        writeln!(w, "Vendor named by the endpoint list against the MIPS-attested vendor, for step-2 practices the lists name:\n")?;
        writeln!(
            w,
            "{}",
            md_table(
                conn,
                r#"
        SELECT CASE WHEN vendor_source = 'lantern' THEN 'MIPS gave no vendor; Lantern fills it'
                    WHEN lantern_vendor = vendor THEN 'same vendor' ELSE 'different' END AS result,
               count(*) AS practices, round(100.0 * count(*) / sum(count(*)) OVER (), 1) AS "%"
        FROM practices WHERE source = 'cms_dac' AND lantern_vendor IS NOT NULL
        GROUP BY 1 ORDER BY 2 DESC"#
            )?
        )?;
        writeln!(w, "Most common disagreements (MIPS vendor → vendor whose list names the practice):\n")?;
        writeln!(
            w,
            "{}",
            md_table(
                conn,
                r#"
        SELECT vendor AS "MIPS vendor", lantern_vendor AS "also listed by", count(*) AS practices,
               round(avg(vendor_share), 2) AS "mean MIPS vendor share"
        FROM practices WHERE source = 'cms_dac' AND vendor_source = 'mips' AND lantern_vendor <> vendor
        GROUP BY 1, 2 ORDER BY 3 DESC LIMIT 12"#
            )?
        )?;
        writeln!(w, "Lantern-only practices by vendor: identified in the Doctors & Clinicians file (name, clinicians and addresses from there) or not, and how many could be placed on the map:\n")?;
        writeln!(
            w,
            "{}",
            md_table(
                conn,
                r#"
        SELECT vendor, count(*) AS practices,
               count(*) FILTER (WHERE practice_id NOT LIKE 'lantern:%') AS "identified in D&C",
               count(*) FILTER (WHERE lat IS NOT NULL) AS "with coordinates",
               count(*) FILTER (WHERE lat IS NULL) AS "no ZIP",
               sum(lantern_n_endpoints) AS endpoints
        FROM practices WHERE source = 'lantern'
        GROUP BY 1 ORDER BY 2 DESC LIMIT 20"#
            )?
        )?;
        writeln!(w, "Fuzzy matches to check (lowest scores):\n")?;
        writeln!(
            w,
            "{}",
            md_table(
                conn,
                r#"
        SELECT f.org_name AS "Lantern organization", f.vendor AS "list vendor", p.name AS "matched practice", p.state,
               p.vendor AS "MIPS vendor", round(f.practice_match_score, 3) AS score
        FROM fhir_endpoints f JOIN practices p USING (practice_id)
        WHERE f.practice_match_method = 'fuzzy_name_state'
        QUALIFY row_number() OVER (PARTITION BY f.practice_id ORDER BY f.practice_match_score) = 1
        ORDER BY f.practice_match_score LIMIT 15"#
            )?
        )?;
    }
    writeln!(w, "### Clinician-side vendor vs inpatient vendor\n")?;
    writeln!(w, "Rank-1 vendor among each hospital's affiliated clinicians (ONC clinician CEHRT PUF × Facility Affiliation) against the hospital's own primary vendor. Disagreement is expected where the medical staff runs an ambulatory system.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT CASE WHEN p.clinician_vendor = p.vendor THEN 'same vendor' ELSE 'different' END AS result,
               count(*) AS hospitals, round(100.0 * count(*) / sum(count(*)) OVER (), 1) AS "%"
        FROM hospital_profile p WHERE p.vendor IS NOT NULL AND p.clinician_vendor IS NOT NULL
        GROUP BY 1 ORDER BY 2 DESC"#
        )?
    )?;
    writeln!(w, "### FHIR endpoint vendor vs inpatient vendor (matched hospitals)\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT CASE WHEN p.fhir_vendor = p.vendor THEN 'same vendor' ELSE 'different' END AS result,
               count(*) AS hospitals, round(100.0 * count(*) / sum(count(*)) OVER (), 1) AS "%"
        FROM hospital_profile p WHERE p.vendor IS NOT NULL AND p.fhir_vendor IS NOT NULL
        GROUP BY 1 ORDER BY 2 DESC"#
        )?
    )?;
    writeln!(w, "### Developer names in enrichment sources not in `config/vendor_map.csv`\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT src, developer, n FROM (
            SELECT 'clinician_cehrt' AS src, developer_name_raw AS developer, count(*) AS n
            FROM clinician_cehrt WHERE vendor_tier = 'unmapped' GROUP BY 1, 2
            UNION ALL
            SELECT 'fhir_endpoints', developer_name_raw, count(*)
            FROM fhir_endpoints f WHERE NOT EXISTS (SELECT 1 FROM ref_vendor_map m WHERE lower(trim(m.developer_name_raw)) = lower(trim(f.developer_name_raw)))
            GROUP BY 1, 2
            UNION ALL
            SELECT 'chpl_products', developer_name_raw, count(*)
            FROM chpl_products c WHERE NOT EXISTS (SELECT 1 FROM ref_vendor_map m WHERE lower(trim(m.developer_name_raw)) = lower(trim(c.developer_name_raw)))
            GROUP BY 1, 2
        ) ORDER BY n DESC LIMIT 40"#
        )?
    )?;
    writeln!(w, "### Certified products in use whose CHPL status is not Active\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT c.certification_status AS status, c.vendor, count(DISTINCT e.ccn) AS hospitals, count(DISTINCT e.chpl_id) AS products
        FROM hospital_ehr e JOIN chpl_products c USING (chpl_id)
        WHERE e.year = (SELECT max(year) FROM hospital_ehr) AND c.certification_status <> 'Active'
        GROUP BY 1, 2 ORDER BY 3 DESC LIMIT 20"#
        )?
    )?;

    // Market report (only meaningful once enrichment is loaded).
    let market = write_market_report(conn, paths)?;
    if let Some(m) = market {
        writeln!(w, "See also `reports/{}` for the market cuts (bed-weighted share, size bands, systems, outcomes).\n", m.file_name().unwrap_or_default().to_string_lossy())?;
    }

    let out = paths.reports.join("data_quality.md");
    std::fs::create_dir_all(&paths.reports)?;
    std::fs::write(&out, md)?;
    Ok(out)
}


const ENRICH_COVERAGE_SQL: &str = r#"
WITH n AS (SELECT count(*) AS hospitals FROM hospitals WHERE in_cms_hgi),
cov AS (
    SELECT 'hospital_npi' AS "table", count(*) AS rows_, count(DISTINCT ccn) FILTER (WHERE ccn IN (SELECT ccn FROM hospitals WHERE in_cms_hgi)) AS ccns FROM hospital_npi
    UNION ALL SELECT 'hospital_enrollment', count(*), count(DISTINCT ccn) FILTER (WHERE ccn IN (SELECT ccn FROM hospitals WHERE in_cms_hgi)) FROM hospital_enrollment
    UNION ALL SELECT 'hospital_geo (street or ZIP)', count(*), count(DISTINCT ccn) FILTER (WHERE ccn IN (SELECT ccn FROM hospitals WHERE in_cms_hgi)) FROM hospital_geo WHERE method <> 'none'
    UNION ALL SELECT 'hospital_cost_reports', count(*), count(DISTINCT ccn) FILTER (WHERE ccn IN (SELECT ccn FROM hospitals WHERE in_cms_hgi)) FROM hospital_cost_reports
    UNION ALL SELECT 'hospital_pos', count(*), count(DISTINCT ccn) FILTER (WHERE ccn IN (SELECT ccn FROM hospitals WHERE in_cms_hgi)) FROM hospital_pos
    UNION ALL SELECT 'hospital_measures', count(*), count(DISTINCT ccn) FILTER (WHERE ccn IN (SELECT ccn FROM hospitals WHERE in_cms_hgi)) FROM hospital_measures
    UNION ALL SELECT 'hospital_medicare_inpatient', count(*), count(DISTINCT ccn) FILTER (WHERE ccn IN (SELECT ccn FROM hospitals WHERE in_cms_hgi)) FROM hospital_medicare_inpatient
    UNION ALL SELECT 'hospital_service_area', count(*), count(DISTINCT ccn) FILTER (WHERE ccn IN (SELECT ccn FROM hospitals WHERE in_cms_hgi)) FROM hospital_service_area
    UNION ALL SELECT 'hospital_systems', count(*), count(DISTINCT ccn) FILTER (WHERE ccn IN (SELECT ccn FROM hospitals WHERE in_cms_hgi)) FROM hospital_systems
    UNION ALL SELECT 'clinician_affiliations', count(*), count(DISTINCT ccn) FILTER (WHERE ccn IN (SELECT ccn FROM hospitals WHERE in_cms_hgi)) FROM clinician_affiliations
    UNION ALL SELECT 'hospital_clinician_vendor', count(*), count(DISTINCT ccn) FILTER (WHERE ccn IN (SELECT ccn FROM hospitals WHERE in_cms_hgi)) FROM hospital_clinician_vendor
    UNION ALL SELECT 'fhir_endpoints (matched)', count(*), count(DISTINCT ccn) FILTER (WHERE ccn IN (SELECT ccn FROM hospitals WHERE in_cms_hgi)) FROM fhir_endpoints WHERE ccn IS NOT NULL
    UNION ALL SELECT 'practice_vendor (MIPS groups, with a vendor)', count(*) FILTER (WHERE vendor IS NOT NULL), NULL FROM practice_vendor
    UNION ALL SELECT 'practices (D&C organisations matched, with a vendor)', count(*) FILTER (WHERE vendor IS NOT NULL), NULL FROM practices WHERE source = 'cms_dac'
    UNION ALL SELECT 'practices (Lantern-only, source = lantern)', count(*), NULL FROM practices WHERE source = 'lantern'
    UNION ALL SELECT 'practice_locations', count(*), NULL FROM practice_locations
    UNION ALL SELECT 'hospital_vendor_year_mu (MU-era hospital-years)', count(*), count(DISTINCT ccn) FILTER (WHERE ccn IN (SELECT ccn FROM hospitals WHERE in_cms_hgi)) FROM hospital_vendor_year_mu
    UNION ALL SELECT 'chpl_products (in hospital_ehr)', count(*), NULL FROM chpl_products c WHERE EXISTS (SELECT 1 FROM hospital_ehr e WHERE e.chpl_id = c.chpl_id)
)
SELECT "table", rows_ AS rows, ccns AS "distinct CCNs",
       CASE WHEN ccns IS NULL THEN NULL ELSE round(100.0 * ccns / n.hospitals, 1) END AS "% of CMS hospitals"
FROM cov, n ORDER BY 1
"#;

/// `reports/market.md`: the market cuts that only make sense once enrichment is loaded.
/// Returns `None` (and writes nothing) when no enrichment table has rows.
fn write_market_report(conn: &Connection, paths: &Paths) -> Result<Option<PathBuf>> {
    let any = conn.query_row(
        "SELECT (SELECT count(*) FROM hospital_cost_reports) + (SELECT count(*) FROM hospital_pos)
              + (SELECT count(*) FROM hospital_systems) + (SELECT count(*) FROM hospital_measures)
              + (SELECT count(*) FROM hospital_clinician_vendor) + (SELECT count(*) FROM fhir_endpoints)",
        [],
        |r| r.get::<_, i64>(0),
    )?;
    if any == 0 {
        return Ok(None);
    }
    let mut md = String::new();
    let w = &mut md;
    writeln!(w, "# EHR Atlas — market report\n")?;
    writeln!(
        w,
        "Generated {} by `ehr-atlas report` from `hospital_profile`. Vendor = primary certified EHR, latest PI year per hospital; beds and revenue from the latest HCRIS cost report per hospital. Regenerate after every build; do not hand-edit.\n",
        Utc::now().format("%Y-%m-%d %H:%M UTC")
    )?;

    writeln!(w, "## 1. Vendor share: hospitals, beds, revenue\n")?;
    writeln!(w, "Hospital count treats a 25-bed critical-access hospital and a 1,000-bed academic center the same; beds and net patient revenue do not.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT vendor,
               count(*) AS hospitals, round(100.0 * count(*) / sum(count(*)) OVER (), 1) AS "% hospitals",
               sum(beds) AS beds, round(100.0 * sum(beds) / sum(sum(beds)) OVER (), 1) AS "% beds",
               round(sum(net_patient_revenue) / 1e9, 1) AS "net patient revenue $B",
               round(100.0 * sum(net_patient_revenue) / sum(sum(net_patient_revenue)) OVER (), 1) AS "% revenue",
               sum(discharges_total) AS discharges
        FROM hospital_profile WHERE vendor IS NOT NULL
        GROUP BY 1 ORDER BY 2 DESC LIMIT 15"#
        )?
    )?;
    writeln!(w, "### Bed-weighted top vendor by state\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        WITH s AS (
            SELECT state, vendor, count(*) AS hospitals, sum(beds) AS beds,
                   round(100.0 * sum(beds) / sum(sum(beds)) OVER (PARTITION BY state), 1) AS pct_beds,
                   row_number() OVER (PARTITION BY state ORDER BY sum(beds) DESC NULLS LAST) AS rn
            FROM hospital_profile WHERE vendor IS NOT NULL AND beds IS NOT NULL GROUP BY 1, 2
        )
        SELECT a.state, sum(a.hospitals) AS hospitals, sum(a.beds) AS beds,
               any_value(a.vendor) FILTER (WHERE a.rn = 1) AS "top vendor by beds",
               any_value(a.pct_beds) FILTER (WHERE a.rn = 1) AS "% beds",
               any_value(a.vendor) FILTER (WHERE a.rn = 2) AS "second",
               any_value(a.pct_beds) FILTER (WHERE a.rn = 2) AS "% beds "
        FROM s a GROUP BY 1 ORDER BY 1"#
        )?
    )?;

    writeln!(w, "## 2. Vendor by hospital size, rurality, teaching status, ownership\n")?;
    writeln!(w, "### By bed band (HCRIS beds)\n")?;
    writeln!(w, "{}", md_table(conn, &pivot_sql("bed_band", "CASE bed_band WHEN '<25' THEN 1 WHEN '25-99' THEN 2 WHEN '100-249' THEN 3 WHEN '250-499' THEN 4 ELSE 5 END"))?)?;
    writeln!(w, "### By rurality (CBSA urban/rural flag from the Provider of Services file)\n")?;
    writeln!(w, "{}", md_table(conn, &pivot_sql("cbsa_urban_rural", "cbsa_urban_rural"))?)?;
    writeln!(w, "### By RUCA class (Medicare inpatient file)\n")?;
    writeln!(w, "{}", md_table(conn, &pivot_sql("ruca_desc", "ruca_desc"))?)?;
    writeln!(w, "### Teaching vs non-teaching (interns & residents FTE > 0)\n")?;
    writeln!(w, "{}", md_table(conn, &pivot_sql("is_teaching", "is_teaching"))?)?;
    writeln!(w, "### By ownership\n")?;
    writeln!(w, "{}", md_table(conn, &pivot_sql("ownership", "ownership"))?)?;

    writeln!(w, "## 3. Health systems\n")?;
    writeln!(w, "### Largest systems and their vendor mix (AHRQ Compendium linkage)\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT health_sys_name AS system, health_sys_state AS st, count(*) AS hospitals,
               count(*) FILTER (WHERE vendor IS NOT NULL) AS "with vendor",
               arg_max(vendor, n) AS "top vendor",
               round(100.0 * max(n) / count(*) FILTER (WHERE vendor IS NOT NULL), 0) AS "% top",
               string_agg(DISTINCT vendor, ', ' ORDER BY vendor) AS vendors
        FROM (
            SELECT p.*, s.health_sys_state,
                   CASE WHEN p.vendor IS NULL THEN 0 ELSE count(*) OVER (PARTITION BY p.health_sys_id, p.vendor) END AS n
            FROM hospital_profile p JOIN hospital_systems s ON s.ccn = p.ccn AND s.compendium_year = p.compendium_year
        )
        GROUP BY 1, 2 ORDER BY 3 DESC LIMIT 40"#
        )?
    )?;
    writeln!(w, "### Systems running more than one core EHR\n")?;
    writeln!(w, "Mixed-vendor systems are where migrations happen; the minority vendor's hospitals are the ones to watch.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT health_sys_name AS system, sum(n) AS hospitals, count(DISTINCT vendor) AS vendors,
               round(100.0 * (sum(n) - max(n)) / sum(n), 0) AS "% not on top vendor",
               string_agg(vendor || ' (' || n || ')', ', ' ORDER BY n DESC) AS mix
        FROM (
            SELECT health_sys_name, vendor, count(*) AS n
            FROM hospital_profile WHERE vendor IS NOT NULL AND health_sys_id IS NOT NULL
            GROUP BY 1, 2
        )
        GROUP BY 1 HAVING count(DISTINCT vendor) > 1 AND sum(n) >= 5 ORDER BY (sum(n) - max(n)) DESC LIMIT 40"#
        )?
    )?;

    writeln!(w, "## 4. Vendor vs outcomes\n")?;
    writeln!(w, "Averages of CMS star rating, HCAHPS star, hospital-wide mortality rate and Medicare spending per beneficiary by vendor, split by bed band so a vendor's size mix does not do the talking. Correlation only: vendors are not randomly assigned to hospitals.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT vendor, bed_band, count(*) AS hospitals,
               round(avg(overall_rating), 2) AS "avg star",
               round(avg(hcahps_star), 2) AS "avg HCAHPS star",
               round(avg(hospital_wide_mortality_rate), 2) AS "avg hospital-wide mortality %",
               round(avg(mspb), 3) AS "avg MSPB"
        FROM hospital_profile
        WHERE vendor IN (SELECT vendor FROM hospital_profile WHERE vendor IS NOT NULL GROUP BY 1 HAVING count(*) >= 50)
          AND bed_band IS NOT NULL
        GROUP BY 1, 2 HAVING count(*) >= 10
        ORDER BY 1, CASE bed_band WHEN '<25' THEN 1 WHEN '25-99' THEN 2 WHEN '100-249' THEN 3 WHEN '250-499' THEN 4 ELSE 5 END"#
        )?
    )?;

    writeln!(w, "## 5. Inpatient vs clinician-side vendor\n")?;
    writeln!(w, "Rows: the hospital's primary certified EHR. Columns: the most common CEHRT vendor among its affiliated clinicians (2024 MIPS). Off-diagonal cells are hospitals whose medical staff attests a different system.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        WITH pairs AS (
            SELECT vendor, clinician_vendor, count(*) AS n
            FROM hospital_profile WHERE vendor IS NOT NULL AND clinician_vendor IS NOT NULL
            GROUP BY 1, 2
        ), ranked AS (
            SELECT *, row_number() OVER (PARTITION BY vendor ORDER BY n DESC) AS rn FROM pairs
        )
        SELECT vendor AS "inpatient vendor",
               sum(n) AS hospitals,
               coalesce(sum(n) FILTER (WHERE clinician_vendor = vendor), 0) AS "same",
               round(100.0 * coalesce(sum(n) FILTER (WHERE clinician_vendor = vendor), 0) / sum(n), 1) AS "% same",
               string_agg(clinician_vendor || ' (' || n || ')', ', ' ORDER BY n DESC) FILTER (WHERE clinician_vendor <> vendor AND rn <= 4) AS "top other clinician vendors"
        FROM ranked GROUP BY 1 ORDER BY 2 DESC LIMIT 12"#
        )?
    )?;

    writeln!(w, "## 6. Signal freshness\n")?;
    writeln!(w, "How old the evidence behind each hospital's vendor is. PI attestation is calendar-year; Lantern endpoints are observed daily; cost reports lag a year or more.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT 'PI attestation year' AS signal, CAST(vendor_year AS VARCHAR) AS value, count(*) AS hospitals FROM hospital_profile WHERE vendor IS NOT NULL GROUP BY 1, 2
        UNION ALL
        SELECT 'FHIR endpoint observed', strftime(fhir_observed_at, '%Y-%m'), count(*) FROM hospital_profile WHERE fhir_observed_at IS NOT NULL GROUP BY 1, 2
        UNION ALL
        SELECT 'Cost report fiscal year', CAST(cost_report_fy AS VARCHAR), count(*) FROM hospital_profile WHERE cost_report_fy IS NOT NULL GROUP BY 1, 2
        UNION ALL
        SELECT 'Clinician CEHRT year', CAST(performance_year AS VARCHAR), count(DISTINCT ccn) FROM hospital_clinician_vendor GROUP BY 1, 2
        ORDER BY 1, 2"#
        )?
    )?;

    if count(conn, "SELECT count(*) FROM practice_vendor")? > 0 {
        write_practices_section(conn, w)?;
    }
    if count(conn, "SELECT count(*) FROM hospital_vendor_year_mu")? > 0 {
        write_history_section(conn, w)?;
    }

    let out = paths.reports.join("market.md");
    std::fs::write(&out, md)?;
    Ok(Some(out))
}

/// Vendor history (#29): Meaningful Use attestations (2011–2016) next to the PI years (2023+).
fn write_history_section(conn: &Connection, w: &mut String) -> Result<()> {
    writeln!(w, "## 8. Vendor history
")?;
    writeln!(w, "Primary vendor per attested hospital-year (`hospital_vendor_history`): Meaningful Use attestations for 2011–2016, Promoting Interoperability for 2023 on. No public hospital-level source covers 2017–2022. The two programs enrolled somewhat different hospitals, and MU ramped up over 2011–2013, so compare shares, not counts, and treat 2011–2012 as partial. Program years with fewer than 100 attesting hospitals (2017 stragglers) are left out.\n")?;
    writeln!(w, "### Share of attesting hospitals by year (%)\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        WITH y AS (SELECT year, count(*) AS n FROM hospital_vendor_history WHERE primary_vendor IS NOT NULL GROUP BY year HAVING count(*) >= 100),
        top AS (SELECT primary_vendor FROM hospital_vendor_history WHERE year = (SELECT max(year) FROM y) AND primary_vendor IS NOT NULL
                GROUP BY 1 ORDER BY count(*) DESC LIMIT 8),
        s AS (SELECT CASE WHEN v.primary_vendor IN (SELECT primary_vendor FROM top) THEN v.primary_vendor ELSE 'Other' END AS vendor,
                     v.year, round(100.0 * count(*) / any_value(y.n), 1) AS pct
              FROM hospital_vendor_history v JOIN y USING (year) WHERE v.primary_vendor IS NOT NULL GROUP BY 1, 2)
        PIVOT s ON year USING any_value(pct) GROUP BY vendor ORDER BY vendor = 'Other', vendor"#
        )?
    )?;
    writeln!(w, "### Hospitals attesting\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT year, era, count(*) AS "hospital-years", count(*) FILTER (WHERE primary_vendor IS NOT NULL) AS "with a vendor",
               count(*) FILTER (WHERE rule IN ('core_product', 'single_vendor')) AS "decided by core product / single vendor"
        FROM hospital_vendor_history GROUP BY 1, 2 ORDER BY 1"#
        )?
    )?;
    writeln!(w, "### Largest flows between the last MU year and the first PI year\n")?;
    writeln!(w, "Hospitals whose primary vendor differs across the 2017–2022 gap, both sides decided by a core product or a single vendor. The switch happened somewhere in between. Vendors are today's owners, so Siemens → Cerner or CPSI → TruBridge renames do not count.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT from_vendor AS "from", to_vendor AS "to", count(*) AS hospitals
        FROM hospital_vendor_changes
        WHERE across_gap AND from_rule IN ('core_product', 'single_vendor') AND to_rule IN ('core_product', 'single_vendor')
        GROUP BY 1, 2 ORDER BY 3 DESC, 1, 2 LIMIT 20"#
        )?
    )?;
    writeln!(w, "### Net change by vendor across the gap\n")?;
    writeln!(w, "Same changes, vendors with at least 5 moves.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        WITH c AS (SELECT * FROM hospital_vendor_changes
                   WHERE across_gap AND from_rule IN ('core_product', 'single_vendor') AND to_rule IN ('core_product', 'single_vendor'))
        SELECT vendor, sum(gained) AS gained, sum(lost) AS lost, sum(gained) - sum(lost) AS net
        FROM (SELECT to_vendor AS vendor, 1 AS gained, 0 AS lost FROM c UNION ALL SELECT from_vendor, 0, 1 FROM c)
        GROUP BY 1 HAVING sum(gained) + sum(lost) >= 5 ORDER BY net DESC, vendor"#
        )?
    )?;
    writeln!(w, "### Changes between adjacent years\n")?;
    writeln!(w, "Within the MU years (and 2023 → 2024). Includes changes where one side was decided by module count or a tie, which are often a hospital listing different add-on modules rather than a real switch.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT from_year || ' → ' || to_year AS years, count(*) AS changes,
               count(*) FILTER (WHERE from_rule IN ('core_product', 'single_vendor') AND to_rule IN ('core_product', 'single_vendor')) AS "both sides firm"
        FROM hospital_vendor_changes WHERE NOT across_gap GROUP BY from_year, to_year ORDER BY from_year"#
        )?
    )?;
    Ok(())
}

/// Practices (#17): the MIPS reporting entity as a practice, from the ONC clinician CEHRT PUF.
fn write_practices_section(conn: &Connection, w: &mut String) -> Result<()> {
    writeln!(w, "## 7. Practices (MIPS groups)\n")?;
    writeln!(w, "Each MIPS reporting entity in the ONC clinician CEHRT public-use file treated as a practice, with the vendor most of its clinicians attested. Only clinicians who reported Promoting Interoperability appear, so groups of 15 or fewer are under-represented. \"Office-based\" means under half of the group's clinicians hold a hospital affiliation in CMS Facility Affiliation data; \"hospital-affiliated\" means half or more.\n")?;
    writeln!(w, "### Vendor share, by practice and by clinician\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        WITH p AS (SELECT * FROM practice_vendor WHERE vendor IS NOT NULL),
        t AS (SELECT count(*) AS n_p, sum(n_core_clinicians) AS n_c FROM p)
        SELECT vendor, count(*) AS practices, round(100.0 * count(*) / t.n_p, 1) AS "% of practices",
               sum(n_core_clinicians) AS clinicians, round(100.0 * sum(n_core_clinicians) / t.n_c, 1) AS "% of clinicians",
               count(*) FILTER (WHERE hospital_affiliated_share < 0.5) AS "office-based practices"
        FROM p, t GROUP BY vendor, t.n_p, t.n_c HAVING count(*) >= 10 ORDER BY practices DESC"#
        )?
    )?;
    writeln!(w, "### Office-based practices only, by size\n")?;
    writeln!(w, "Groups where under half the clinicians hold a hospital affiliation. Size is the number of distinct clinicians in the file for that group.\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        WITH p AS (SELECT * FROM practice_vendor WHERE vendor IS NOT NULL AND hospital_affiliated_share < 0.5),
        top AS (SELECT vendor FROM p GROUP BY 1 ORDER BY count(*) DESC LIMIT 6),
        base AS (
            SELECT CASE WHEN n_clinicians <= 5 THEN '1-5' WHEN n_clinicians <= 15 THEN '6-15' WHEN n_clinicians <= 50 THEN '16-50'
                        WHEN n_clinicians <= 200 THEN '51-200' ELSE '200+' END AS size,
                   CASE WHEN n_clinicians <= 5 THEN 1 WHEN n_clinicians <= 15 THEN 2 WHEN n_clinicians <= 50 THEN 3 WHEN n_clinicians <= 200 THEN 4 ELSE 5 END AS ord,
                   vendor FROM p
        ), tot AS (SELECT size, ord, count(*) AS practices FROM base GROUP BY 1, 2),
        pv AS (SELECT size, vendor, round(100.0 * count(*) / sum(count(*)) OVER (PARTITION BY size), 1) AS pct FROM base GROUP BY 1, 2)
        SELECT t.size AS "clinicians in group", t.practices,
               (SELECT string_agg(vendor || ' ' || pct || '%', ', ' ORDER BY pct DESC) FROM pv
                WHERE pv.size = t.size AND pv.vendor IN (SELECT vendor FROM top)) AS "top vendors"
        FROM tot t ORDER BY t.ord"#
        )?
    )?;
    writeln!(w, "### Office-based practices by state (top 15 states)\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        WITH p AS (SELECT * FROM practice_vendor WHERE vendor IS NOT NULL AND hospital_affiliated_share < 0.5 AND practice_state IS NOT NULL),
        top AS (SELECT vendor FROM p GROUP BY 1 ORDER BY count(*) DESC LIMIT 5),
        tot AS (SELECT practice_state, count(*) AS practices, sum(n_core_clinicians) AS clinicians FROM p GROUP BY 1),
        pv AS (SELECT practice_state, vendor, round(100.0 * count(*) / sum(count(*)) OVER (PARTITION BY practice_state), 1) AS pct FROM p GROUP BY 1, 2)
        SELECT t.practice_state AS state, t.practices, t.clinicians,
               (SELECT string_agg(vendor || ' ' || pct || '%', ', ' ORDER BY pct DESC) FROM pv
                WHERE pv.practice_state = t.practice_state AND pv.vendor IN (SELECT vendor FROM top)) AS "top vendors"
        FROM tot t ORDER BY t.practices DESC LIMIT 15"#
        )?
    )?;
    if count(conn, "SELECT count(*) FROM practices WHERE vendor IS NOT NULL")? > 0 {
        writeln!(w, "### Largest practices (CMS Doctors & Clinicians organisations)\n")?;
        writeln!(w, "MIPS groups matched to their organisation in the Doctors & Clinicians file (#18). Clinicians are everyone the file lists under the organisation; the vendor and its share come from the group's clinicians who reported PI.\n")?;
        writeln!(
            w,
            "{}",
            md_table(
                conn,
                r#"
        SELECT name AS practice, state, n_clinicians AS clinicians, n_locations AS locations, vendor,
               round(100 * vendor_share) AS "% of PI clinicians", n_mips_clinicians AS "PI clinicians",
               CASE WHEN hospital_affiliated_share >= 0.5 THEN 'hospital-based' ELSE 'office-based' END AS setting
        FROM practices WHERE vendor IS NOT NULL AND source = 'cms_dac' ORDER BY n_clinicians DESC LIMIT 25"#
            )?
        )?;
        if count(conn, "SELECT count(*) FROM practices WHERE source = 'lantern'")? > 0 {
            writeln!(w, "### Vendor share including practices known only from FHIR endpoint lists (#19)\n")?;
            writeln!(w, "Step-2 practices carry an attested (MIPS) vendor; Lantern-only practices are organizations a vendor's own endpoint list names, with no attestation behind them, and they cover practices outside MIPS (many of them small). Counts of organizations, not clinicians; a practice a list names under several departments counts once per NPI.\n")?;
            writeln!(
                w,
                "{}",
                md_table(
                    conn,
                    r#"
        SELECT vendor,
               count(*) FILTER (WHERE source = 'cms_dac') AS "MIPS practices",
               count(*) FILTER (WHERE source = 'lantern') AS "Lantern-only practices",
               count(*) AS total, round(100.0 * count(*) / sum(count(*)) OVER (), 1) AS "% of total",
               count(*) FILTER (WHERE source = 'cms_dac' AND lantern_vendor = vendor) AS "MIPS practices the list confirms"
        FROM practices WHERE vendor IS NOT NULL
        GROUP BY 1 HAVING count(*) >= 25 ORDER BY total DESC"#
                )?
            )?;
        }
        writeln!(w, "### Vendor share by practice specialty (top specialties, office-based practices)\n")?;
        writeln!(
            w,
            "{}",
            md_table(
                conn,
                r#"
        WITH p AS (SELECT * FROM practices WHERE vendor IS NOT NULL AND source = 'cms_dac' AND hospital_affiliated_share < 0.5 AND top_specialty IS NOT NULL),
        spec AS (SELECT top_specialty FROM p GROUP BY 1 ORDER BY count(*) DESC LIMIT 12),
        top AS (SELECT vendor FROM p GROUP BY 1 ORDER BY count(*) DESC LIMIT 5),
        pv AS (SELECT top_specialty, vendor, round(100.0 * count(*) / sum(count(*)) OVER (PARTITION BY top_specialty), 1) AS pct FROM p GROUP BY 1, 2)
        SELECT s.top_specialty AS specialty, (SELECT count(*) FROM p WHERE p.top_specialty = s.top_specialty) AS practices,
               (SELECT string_agg(pv.vendor || ' ' || pv.pct || '%', ', ' ORDER BY pv.pct DESC) FROM pv
                WHERE pv.top_specialty = s.top_specialty AND pv.vendor IN (SELECT vendor FROM top)) AS "top vendors"
        FROM spec s ORDER BY practices DESC"#
            )?
        )?;
    }
    writeln!(w, "### Mixed-vendor and unresolved groups\n")?;
    writeln!(
        w,
        "{}",
        md_table(
            conn,
            r#"
        SELECT 'groups in file' AS measure, count(*) AS n FROM practice_vendor
        UNION ALL SELECT 'with a core-tier vendor', count(*) FROM practice_vendor WHERE vendor IS NOT NULL
        UNION ALL SELECT 'majority vendor below 60% of clinicians', count(*) FROM practice_vendor WHERE vendor_share < 0.6
        UNION ALL SELECT 'two or more core vendors', count(*) FROM practice_vendor WHERE n_vendors >= 2
        UNION ALL SELECT 'ties (picked alphabetically)', count(*) FROM practice_vendor WHERE is_tie
        UNION ALL SELECT 'office-based (< 50% hospital-affiliated)', count(*) FROM practice_vendor WHERE vendor IS NOT NULL AND hospital_affiliated_share < 0.5
        UNION ALL SELECT 'hospital-affiliated (>= 50%)', count(*) FROM practice_vendor WHERE vendor IS NOT NULL AND hospital_affiliated_share >= 0.5"#
        )?
    )?;
    Ok(())
}

/// Vendor share pivot: one row per value of `dim`, one column per top vendor, hospitals with a vendor only.
fn pivot_sql(dim: &str, order: &str) -> String {
    format!(
        r#"
        WITH top AS (
            SELECT vendor FROM hospital_profile WHERE vendor IS NOT NULL GROUP BY 1 ORDER BY count(*) DESC LIMIT 6
        ), base AS (
            SELECT CAST({dim} AS VARCHAR) AS dim, {order} AS ord, vendor FROM hospital_profile WHERE vendor IS NOT NULL AND {dim} IS NOT NULL
        )
        SELECT dim AS "{dim}", count(*) AS hospitals,
               {cols},
               round(100.0 * count(*) FILTER (WHERE vendor NOT IN (SELECT vendor FROM top)) / count(*), 1) AS "other %"
        FROM base GROUP BY dim, ord ORDER BY ord, dim"#,
        cols = ["Epic", "Oracle Health", "MEDITECH", "TruBridge", "Altera Digital Health", "MEDHOST"]
            .iter()
            .map(|v| format!(
                r#"round(100.0 * count(*) FILTER (WHERE vendor = '{v}') / count(*), 1) AS "{v} %""#
            ))
            .collect::<Vec<_>>()
            .join(",\n               ")
    )
}
