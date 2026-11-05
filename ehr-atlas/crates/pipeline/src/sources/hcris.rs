//! data.cms.gov — Hospital Provider Cost Report (HCRIS), one CSV per fiscal year.
//!
//! Beds, discharges, FTEs, revenue and costs per hospital fiscal year. A CCN can file
//! two reports in one year (a short report when its fiscal year changes); the longest
//! period wins, then the latest fiscal-year end.

use crate::config::Paths;
use crate::db::{count, lit};
use crate::download::{
    all_from_manifest, check_csv_header, data_cms_gov_distributions, download, DownloadRequest,
};
use anyhow::{ensure, Context, Result};
use duckdb::Connection;
use ehr_atlas_core::manifest::RawFile;
use ehr_atlas_core::sources::{SourceId, HCRIS_DATASET_UUID, HCRIS_REQUIRED_COLUMNS};
use tracing::info;

pub const SOURCE: &str = "hcris";
/// How many fiscal years to load (newest first).
const YEARS: usize = 3;

fn year_of(temporal: &str) -> Option<u16> {
    temporal.get(0..4)?.parse().ok()
}

pub async fn fetch(paths: &Paths, client: &reqwest::Client, offline: bool) -> Result<Vec<RawFile>> {
    if offline {
        return all_from_manifest(paths, SOURCE);
    }
    let dists = data_cms_gov_distributions(client, HCRIS_DATASET_UUID).await?;
    let mut out = Vec::new();
    for d in dists.iter().filter(|d| year_of(&d.temporal).is_some()).take(YEARS) {
        let year = year_of(&d.temporal).unwrap();
        let rf = download(
            paths,
            client,
            DownloadRequest {
                source: SourceId::Hcris.as_str(),
                url: &d.download_url,
                file_name: &format!("CostReport_{year}.csv"),
                upstream_modified: d.modified.clone(),
                notes: Some(format!(
                    "data.cms.gov dataset {HCRIS_DATASET_UUID}: {} ({}); fiscal_year={year}",
                    d.title, d.temporal
                )),
            },
        )
        .await?;
        out.push(rf);
    }
    ensure!(!out.is_empty(), "no HCRIS fiscal-year distributions found");
    Ok(out)
}

/// Fiscal year from the manifest notes (`fiscal_year=YYYY`), falling back to the file name.
fn fiscal_year(file: &RawFile) -> Result<u16> {
    if let Some(n) = file.notes.as_deref().and_then(|n| n.split("fiscal_year=").nth(1)) {
        if let Ok(y) = n.trim().get(0..4).unwrap_or("").parse() {
            return Ok(y);
        }
    }
    let name = file.path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    name.split("CostReport_")
        .nth(1)
        .and_then(|s| s.get(0..4))
        .and_then(|s| s.parse().ok())
        .with_context(|| format!("cannot infer fiscal year of {}", file.path.display()))
}

pub fn normalize(conn: &Connection, paths: &Paths, files: &[RawFile]) -> Result<()> {
    conn.execute_batch("DELETE FROM stg_hcris")?;
    // Load every file we have (older manifest entries included); the newest file for a
    // fiscal year wins because rows are inserted oldest-first and de-duplicated below.
    let mut files: Vec<&RawFile> = files.iter().collect();
    files.sort_by_key(|f| f.downloaded_at);
    for file in files {
        let path = paths.root.join(&file.path);
        check_csv_header(&path, HCRIS_REQUIRED_COLUMNS)?;
        let fy = fiscal_year(file)?;
        conn.execute_batch(&format!(
            r#"
            DELETE FROM stg_hcris WHERE fiscal_year = {fy};
            INSERT INTO stg_hcris
            SELECT
                trim("Provider CCN")                                            AS ccn,
                {fy}                                                            AS fiscal_year,
                TRY_CAST(strptime(nullif("Fiscal Year Begin Date", 'NULL'), '%m/%d/%Y') AS DATE) AS fy_begin,
                TRY_CAST(strptime(nullif("Fiscal Year End Date", 'NULL'), '%m/%d/%Y') AS DATE)   AS fy_end,
                nullif(trim("City"), 'NULL')                                    AS city,
                nullif(trim("State Code"), 'NULL')                              AS state,
                nullif(trim("CCN Facility Type"), 'NULL')                       AS facility_type,
                TRY_CAST(nullif("FTE - Employees on Payroll", 'NULL') AS DOUBLE)           AS fte_employees,
                TRY_CAST(nullif("Number of Interns and Residents (FTE)", 'NULL') AS DOUBLE) AS interns_residents_fte,
                TRY_CAST(TRY_CAST(nullif("Number of Beds", 'NULL') AS DOUBLE) AS INTEGER)  AS beds,
                TRY_CAST(TRY_CAST(nullif("Total Bed Days Available", 'NULL') AS DOUBLE) AS BIGINT) AS bed_days_available,
                TRY_CAST(TRY_CAST(nullif("Total Discharges (V + XVIII + XIX + Unknown)", 'NULL') AS DOUBLE) AS INTEGER) AS discharges_total,
                TRY_CAST(TRY_CAST(nullif("Total Discharges Title XVIII", 'NULL') AS DOUBLE) AS INTEGER) AS discharges_medicare,
                TRY_CAST(TRY_CAST(nullif("Total Discharges Title XIX", 'NULL') AS DOUBLE) AS INTEGER)   AS discharges_medicaid,
                TRY_CAST(nullif("Inpatient Revenue", 'NULL') AS DOUBLE)         AS inpatient_revenue,
                TRY_CAST(nullif("Outpatient Revenue", 'NULL') AS DOUBLE)        AS outpatient_revenue,
                TRY_CAST(nullif("Total Patient Revenue", 'NULL') AS DOUBLE)     AS total_patient_revenue,
                TRY_CAST(nullif("Net Patient Revenue", 'NULL') AS DOUBLE)       AS net_patient_revenue,
                TRY_CAST(nullif("Total Costs", 'NULL') AS DOUBLE)               AS total_costs,
                TRY_CAST(nullif("Cost To Charge Ratio", 'NULL') AS DOUBLE)      AS cost_to_charge_ratio,
                TRY_CAST(nullif("Cost of Charity Care", 'NULL') AS DOUBLE)      AS charity_care_cost,
                TRY_CAST(nullif("Cost of Uncompensated Care", 'NULL') AS DOUBLE) AS uncompensated_care_cost,
                TRY_CAST(nullif("Net Revenue from Medicaid", 'NULL') AS DOUBLE) AS medicaid_net_revenue,
                {fid} AS source_file_id
            FROM read_csv({p}, header=true, all_varchar=true)
            WHERE trim("Provider CCN") <> '';
            "#,
            p = lit(&path.to_string_lossy()),
            fid = lit(&file.id),
        ))?;
        let n = count(conn, &format!("SELECT count(*) FROM stg_hcris WHERE fiscal_year = {fy}"))?;
        info!(fiscal_year = fy, rows = n, file = %file.path.display(), "stg_hcris loaded");
    }
    let n = count(conn, "SELECT count(*) FROM stg_hcris")?;
    ensure!(n > 0, "stg_hcris is empty after loading");
    Ok(())
}

pub fn derive(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        DELETE FROM hospital_cost_reports;
        INSERT INTO hospital_cost_reports
        SELECT ccn, fiscal_year, fy_begin, fy_end, beds, bed_days_available,
               discharges_total, discharges_medicare, discharges_medicaid,
               fte_employees, interns_residents_fte,
               inpatient_revenue, outpatient_revenue, total_patient_revenue, net_patient_revenue,
               total_costs, cost_to_charge_ratio, charity_care_cost, uncompensated_care_cost,
               medicaid_net_revenue, source_file_id
        FROM stg_hcris
        QUALIFY row_number() OVER (
            PARTITION BY ccn, fiscal_year
            ORDER BY (fy_end - fy_begin) DESC NULLS LAST, fy_end DESC NULLS LAST, source_file_id
        ) = 1;
        "#,
    )?;
    let n = count(conn, "SELECT count(*) FROM hospital_cost_reports")?;
    let dup = count(
        conn,
        "SELECT count(*) FROM (SELECT ccn, fiscal_year FROM hospital_cost_reports GROUP BY 1,2 HAVING count(*) > 1)",
    )?;
    ensure!(dup == 0, "hospital_cost_reports has {dup} duplicate (ccn, fiscal_year) keys");
    let mut stmt = conn.prepare(
        r#"SELECT fiscal_year,
                  count(*) AS reports,
                  count(*) FILTER (WHERE EXISTS (SELECT 1 FROM hospitals h WHERE h.ccn = c.ccn AND h.in_cms_hgi)) AS in_cms,
                  (SELECT count(*) FROM hospitals WHERE in_cms_hgi) AS cms_total
           FROM hospital_cost_reports c GROUP BY 1 ORDER BY 1"#,
    )?;
    let rows: Vec<(i32, i64, i64, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    for (fy, reports, in_cms, total) in rows {
        info!(
            fiscal_year = fy,
            reports,
            cms_hospitals_covered = in_cms,
            coverage_pct = format!("{:.1}", 100.0 * in_cms as f64 / total.max(1) as f64),
            "hospital_cost_reports coverage"
        );
    }
    info!(hospital_cost_reports = n, "cost reports derived");
    Ok(())
}
