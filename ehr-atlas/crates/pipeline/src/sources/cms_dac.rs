//! Practice identity (#18): CMS Provider Data Catalog **Doctors and Clinicians National
//! Downloadable File** (dataset `mj5m-pzi6`, ~2.7M rows, ~840 MB). One row per clinician ×
//! Medicare enrollment × group × practice address: NPI, individual PAC ID, name,
//! credential, primary/secondary specialty, group legal name (`Facility Name`), group PAC
//! ID (`org_pac_id`), group member count, street address, phone and CMS's address key
//! (`adrs_id`).
//!
//! # Matching MIPS groups to organisations
//!
//! `practice_vendor` (from the ONC clinician CEHRT PUF, #17) is keyed by ONC's opaque
//! `group_key`. Each group is a set of clinician NPIs; in D&C each NPI belongs to zero or
//! more organisations. For every group we count how many of its clinicians D&C lists under
//! each organisation and take the plurality — ties broken by a location in the group's
//! practice state (the PUF's state is not reliable enough to rank on), then by the closest
//! published member count to the group's reported size. The practice is `org:<org_pac_id>`. A single-clinician group whose
//! clinician has no organisation in D&C becomes `ind:<ind_pac_id>` (a solo enrollment).
//! Groups with no clinician in D&C at all stay unmatched (`practice_id IS NULL`).
//!
//! Several MIPS groups can map to one organisation (a group plus clinicians who reported
//! individually under the same TIN, or an APM entity); `practices` recomputes the vendor
//! over the union of their clinicians and keeps `n_groups`.
//!
//! # Locations
//!
//! Every distinct address of the organisation's members (all of them, not just the MIPS
//! reporters) is a `practice_locations` row, ranked by clinician count; rank 1 is the
//! practice's primary location and the one plotted on the map. The top
//! [`MAX_GEOCODED_LOCATIONS`] per practice go through the Census batch geocoder (the
//! `geocode` source picks them up through [`stage_practice_locations`]); the rest fall back
//! to the ZIP centroid.

use crate::config::Paths;
use crate::db::{count, lit};
use crate::download::{all_from_manifest, check_csv_header, cms_pdc_distribution, download, DownloadRequest};
use anyhow::{ensure, Result};
use duckdb::Connection;
use ehr_atlas_core::manifest::RawFile;
use ehr_atlas_core::sources::{SourceId, CMS_DAC_DATASET_ID, CMS_DAC_REQUIRED_COLUMNS};
use tracing::info;

pub const SOURCE: &str = "cms_dac";
/// Locations per practice sent to the Census geocoder; lower-ranked ones use ZIP centroids.
pub const MAX_GEOCODED_LOCATIONS: i64 = 20;
/// An organisation match is kept when it covers at least [`MIN_MATCH_SHARE`] of the group's
/// clinicians, or at least [`MIN_MATCH_CLINICIANS`] clinicians covering [`MIN_MATCH_SHARE_LARGE`]
/// (large groups whose clinicians enrol under several entities).
const MIN_MATCH_SHARE: f64 = 0.2;
const MIN_MATCH_CLINICIANS: i64 = 3;
const MIN_MATCH_SHARE_LARGE: f64 = 0.05;

pub async fn fetch(paths: &Paths, client: &reqwest::Client, offline: bool) -> Result<Vec<RawFile>> {
    if offline {
        let files = all_from_manifest(paths, SOURCE)?;
        return Ok(vec![files.last().cloned().expect("non-empty")]);
    }
    let (url, modified, title) = cms_pdc_distribution(client, CMS_DAC_DATASET_ID).await?;
    let rf = download(
        paths,
        client,
        DownloadRequest {
            source: SourceId::CmsDac.as_str(),
            url: &url,
            file_name: "DAC_NationalDownloadableFile.csv",
            upstream_modified: modified,
            notes: Some(format!("CMS PDC dataset {CMS_DAC_DATASET_ID}: {title}")),
        },
    )
    .await?;
    Ok(vec![rf])
}

pub fn normalize(conn: &Connection, paths: &Paths, files: &[RawFile]) -> Result<()> {
    let file = files.last().expect("fetch returns one file");
    let path = paths.root.join(&file.path);
    check_csv_header(&path, CMS_DAC_REQUIRED_COLUMNS)?;
    conn.execute_batch(&format!(
        r#"
        DELETE FROM stg_cms_dac;
        INSERT INTO stg_cms_dac
        SELECT trim("NPI")                                        AS npi,
               nullif(trim("Ind_PAC_ID"), '')                     AS ind_pac_id,
               nullif(trim("Ind_enrl_ID"), '')                    AS ind_enrl_id,
               nullif(trim("Provider Last Name"), '')             AS last_name,
               nullif(trim("Provider First Name"), '')            AS first_name,
               nullif(trim("Provider Middle Name"), '')           AS middle_name,
               nullif(trim("suff"), '')                           AS suffix,
               nullif(trim("gndr"), '')                           AS gender,
               nullif(trim("Cred"), '')                           AS credential,
               nullif(trim("Med_sch"), '')                        AS med_school,
               TRY_CAST(nullif(trim("Grd_yr"), '') AS SMALLINT)   AS grad_year,
               nullif(trim("pri_spec"), '')                       AS primary_specialty,
               nullif(trim("sec_spec_all"), '')                   AS secondary_specialties,
               CASE upper(trim("Telehlth")) WHEN 'Y' THEN TRUE WHEN 'N' THEN FALSE END AS telehealth,
               nullif(trim("Facility Name"), '')                  AS org_name,
               nullif(trim("org_pac_id"), '')                     AS org_pac_id,
               TRY_CAST(nullif(trim("num_org_mem"), '') AS INTEGER) AS org_members,
               nullif(trim("adr_ln_1"), '')                       AS address1,
               nullif(trim("adr_ln_2"), '')                       AS address2,
               CASE upper(trim("ln_2_sprs")) WHEN 'Y' THEN TRUE WHEN 'N' THEN FALSE END AS address2_suppressed,
               nullif(trim("City/Town"), '')                      AS city,
               nullif(trim("State"), '')                          AS state,
               nullif(regexp_replace(trim("ZIP Code"), '[^0-9]', '', 'g'), '') AS zip,
               nullif(regexp_replace(trim("Telephone Number"), '[^0-9]', '', 'g'), '') AS phone,
               nullif(trim("ind_assgn"), '')                      AS ind_assignment,
               nullif(trim("grp_assgn"), '')                      AS group_assignment,
               nullif(trim("adrs_id"), '')                        AS address_id,
               {fid}                                              AS source_file_id
        FROM read_csv({p}, header=true, all_varchar=true)
        WHERE trim("NPI") <> '';
        "#,
        p = lit(&path.to_string_lossy()),
        fid = lit(&file.id),
    ))?;
    let n = count(conn, "SELECT count(*) FROM stg_cms_dac")?;
    let npis = count(conn, "SELECT count(DISTINCT npi) FROM stg_cms_dac")?;
    let orgs = count(conn, "SELECT count(DISTINCT org_pac_id) FROM stg_cms_dac")?;
    let addrs = count(conn, "SELECT count(DISTINCT address_id) FROM stg_cms_dac")?;
    info!(rows = n, clinicians = npis, organisations = orgs, addresses = addrs, "stg_cms_dac loaded");
    ensure!(n > 0, "Doctors & Clinicians file loaded zero rows");
    Ok(())
}

/// Build the temp tables `_practice_match` (group_key, performance_year → practice_id) and
/// `_practice_loc` (practice_id × address, ranked) from staging. Shared by [`derive`] and by
/// the geocoder's candidate list. Returns `false` (and builds nothing) when either the D&C
/// file or the clinician PUF is not loaded.
pub fn stage_practice_locations(conn: &Connection) -> Result<bool> {
    if count(conn, "SELECT count(*) FROM stg_cms_dac")? == 0
        || count(conn, "SELECT count(*) FROM stg_onc_clinician_cehrt")? == 0
    {
        return Ok(false);
    }
    conn.execute_batch(&format!(
        r#"
        CREATE OR REPLACE TEMP TABLE _grp AS
        SELECT group_key, performance_year, mode(practice_state) AS practice_state,
               max(practice_size) AS practice_size, count(DISTINCT npi) AS n_clinicians
        FROM stg_onc_clinician_cehrt WHERE group_key IS NOT NULL GROUP BY 1, 2;

        CREATE OR REPLACE TEMP TABLE _grp_npi AS
        SELECT DISTINCT group_key, performance_year, npi
        FROM stg_onc_clinician_cehrt WHERE group_key IS NOT NULL;

        CREATE OR REPLACE TEMP TABLE _dac_org AS
        SELECT npi, org_pac_id, state FROM stg_cms_dac WHERE org_pac_id IS NOT NULL GROUP BY 1, 2, 3;

        CREATE OR REPLACE TEMP TABLE _org_size AS
        SELECT org_pac_id, max(org_members) AS org_members FROM stg_cms_dac WHERE org_pac_id IS NOT NULL GROUP BY 1;

        -- every organisation that lists at least one of the group's clinicians
        CREATE OR REPLACE TEMP TABLE _cand AS
        SELECT n.group_key, n.performance_year, o.org_pac_id,
               count(DISTINCT n.npi) AS n_match,
               count(DISTINCT n.npi) FILTER (WHERE o.state = g.practice_state) AS n_match_state
        FROM _grp_npi n
        JOIN _grp g USING (group_key, performance_year)
        JOIN _dac_org o ON o.npi = n.npi
        GROUP BY 1, 2, 3;

        CREATE OR REPLACE TEMP TABLE _practice_match AS
        WITH ranked AS (
            SELECT c.group_key, c.performance_year, c.org_pac_id, c.n_match, g.n_clinicians,
                   row_number() OVER (PARTITION BY c.group_key, c.performance_year
                                      ORDER BY c.n_match DESC, c.n_match_state DESC,
                                               abs(coalesce(s.org_members, 0) - coalesce(g.practice_size, 0)),
                                               c.org_pac_id) AS rn
            FROM _cand c
            JOIN _grp g USING (group_key, performance_year)
            LEFT JOIN _org_size s USING (org_pac_id)
        ), org AS (
            SELECT group_key, performance_year, 'org:' || org_pac_id AS practice_id, org_pac_id,
                   CAST(NULL AS TEXT) AS ind_pac_id, n_match, n_match / n_clinicians AS match_share, 'org' AS match_method
            FROM ranked
            WHERE rn = 1 AND (n_match / n_clinicians >= {min_share} OR (n_match >= {min_n} AND n_match / n_clinicians >= {min_share_large}))
        ), solo AS (
            SELECT g.group_key, g.performance_year, 'ind:' || d.ind_pac_id, NULL, d.ind_pac_id, 1, 1.0, 'individual'
            FROM _grp g
            JOIN _grp_npi n USING (group_key, performance_year)
            JOIN (SELECT npi, min(ind_pac_id) AS ind_pac_id FROM stg_cms_dac
                  WHERE org_pac_id IS NULL AND ind_pac_id IS NOT NULL GROUP BY 1) d ON d.npi = n.npi
            WHERE g.n_clinicians = 1
              AND NOT EXISTS (SELECT 1 FROM org o WHERE o.group_key = g.group_key AND o.performance_year = g.performance_year)
        )
        SELECT * FROM org UNION ALL SELECT * FROM solo;

        -- every address of the practice's members (organisation members, or the solo clinician's own rows)
        CREATE OR REPLACE TEMP TABLE _practice_loc AS
        {loc};

        DROP TABLE _grp; DROP TABLE _grp_npi; DROP TABLE _dac_org; DROP TABLE _org_size; DROP TABLE _cand;
        "#,
        min_share = MIN_MATCH_SHARE,
        min_n = MIN_MATCH_CLINICIANS,
        min_share_large = MIN_MATCH_SHARE_LARGE,
        loc = dac_locations_sql("(SELECT DISTINCT practice_id, org_pac_id, ind_pac_id FROM _practice_match)"),
    ))?;
    Ok(true)
}

/// Street addresses the geocoder should resolve: the top-ranked locations of every practice.
pub fn geocode_candidates_sql() -> String {
    format!(
        "SELECT address1, city, state, zip FROM _practice_loc WHERE loc_rank <= {MAX_GEOCODED_LOCATIONS}"
    )
}

/// Every distinct D&C address of the practices in `ids` — a table or parenthesised subquery with
/// columns `practice_id`, `org_pac_id`, `ind_pac_id` (one of the two set) — as
/// `(practice_id, address_id, address1, address2, city, state, zip, phone, n_clinicians, loc_rank)`,
/// rank 1 being the address with the most clinicians.
pub fn dac_locations_sql(ids: &str) -> String {
    format!(
        r#"
        WITH rows_ AS (
            SELECT i.practice_id, d.address_id, d.address1,
                   CASE WHEN d.address2_suppressed THEN NULL ELSE d.address2 END AS address2,
                   d.city, d.state, d.zip, d.phone, d.npi
            FROM {ids} i JOIN stg_cms_dac d ON d.org_pac_id = i.org_pac_id
            WHERE i.org_pac_id IS NOT NULL
            UNION ALL
            SELECT i.practice_id, d.address_id, d.address1,
                   CASE WHEN d.address2_suppressed THEN NULL ELSE d.address2 END,
                   d.city, d.state, d.zip, d.phone, d.npi
            FROM {ids} i JOIN stg_cms_dac d ON d.ind_pac_id = i.ind_pac_id AND d.org_pac_id IS NULL
            WHERE i.ind_pac_id IS NOT NULL
        )
        SELECT practice_id, address_id,
               any_value(address1) AS address1, any_value(address2) AS address2,
               any_value(city) AS city, any_value(state) AS state, any_value(zip) AS zip, any_value(phone) AS phone,
               count(DISTINCT npi) AS n_clinicians,
               row_number() OVER (PARTITION BY practice_id ORDER BY count(DISTINCT npi) DESC, address_id) AS loc_rank
        FROM rows_
        WHERE address_id IS NOT NULL AND address1 IS NOT NULL AND city IS NOT NULL AND state IS NOT NULL
        GROUP BY 1, 2"#
    )
}

/// Name and size of the practices in `ids` (see [`dac_locations_sql`]) from D&C:
/// `(practice_id, name, n_members, n_clinicians, source_file_id)`.
pub fn dac_identity_sql(ids: &str) -> String {
    format!(
        r#"
        SELECT 'org:' || d.org_pac_id AS practice_id, any_value(d.org_name) AS name,
               max(d.org_members) AS n_members, count(DISTINCT d.npi) AS n_clinicians,
               any_value(d.source_file_id) AS source_file_id
        FROM stg_cms_dac d JOIN {ids} i ON i.org_pac_id = d.org_pac_id GROUP BY 1
        UNION ALL
        SELECT 'ind:' || d.ind_pac_id,
               any_value(concat_ws(' ', d.first_name, d.last_name) || coalesce(', ' || d.credential, '')),
               1, count(DISTINCT d.npi), any_value(d.source_file_id)
        FROM stg_cms_dac d JOIN {ids} i ON i.ind_pac_id = d.ind_pac_id AND d.org_pac_id IS NULL GROUP BY 1"#
    )
}

/// Top specialty and the top-five mix of the practices in `ids` (see [`dac_locations_sql`]):
/// `(practice_id, top_specialty, specialty_mix)`.
pub fn dac_specialty_sql(ids: &str) -> String {
    format!(
        r#"
        WITH spec_rows AS (
            SELECT i.practice_id, d.primary_specialty AS spec, count(DISTINCT d.npi) AS n
            FROM {ids} i JOIN stg_cms_dac d ON d.org_pac_id = i.org_pac_id
            WHERE d.primary_specialty IS NOT NULL GROUP BY 1, 2
            UNION ALL
            SELECT i.practice_id, d.primary_specialty, count(DISTINCT d.npi)
            FROM {ids} i JOIN stg_cms_dac d ON d.ind_pac_id = i.ind_pac_id AND d.org_pac_id IS NULL
            WHERE i.ind_pac_id IS NOT NULL AND d.primary_specialty IS NOT NULL GROUP BY 1, 2
        )
        SELECT practice_id, arg_max(spec, n) AS top_specialty,
               string_agg(spec || ':' || n, '|' ORDER BY n DESC, spec) AS specialty_mix
        FROM (SELECT * FROM spec_rows QUALIFY row_number() OVER (PARTITION BY practice_id ORDER BY n DESC, spec) <= 5)
        GROUP BY 1"#
    )
}

pub fn derive(conn: &Connection) -> Result<()> {
    ensure!(
        count(conn, "SELECT count(*) FROM practice_vendor")? > 0,
        "practice_vendor is empty; the clinicians derive step must run before cms_dac"
    );
    ensure!(stage_practice_locations(conn)?, "stg_cms_dac or stg_onc_clinician_cehrt is empty");
    let key = |street: &str, city: &str, state: &str, zip: &str| {
        crate::sources::geocode::address_key_sql(street, city, state, zip)
    };
    conn.execute_batch(&format!(
        r#"
        UPDATE practice_vendor v
        SET practice_id = m.practice_id, match_method = m.match_method, match_share = m.match_share
        FROM _practice_match m
        WHERE m.group_key = v.group_key AND m.performance_year = v.performance_year;

        -- locations with coordinates: Census street match for the geocoded ranks, ZIP centroid otherwise
        CREATE OR REPLACE TEMP TABLE _loc_geo AS
        WITH keyed AS (
            -- only the geocoded ranks get a key, so the join stays a plain hash join
            SELECT l.*, CASE WHEN l.loc_rank <= {max_geo} THEN {akey} END AS gkey FROM _practice_loc l
        )
        SELECT l.* EXCLUDE (gkey),
               coalesce(g.lat, z.lat) AS lat, coalesce(g.lon, z.lon) AS lon,
               CASE WHEN g.lat IS NOT NULL AND g.match_type = 'Exact' THEN 'census_exact'
                    WHEN g.lat IS NOT NULL THEN 'census_nonexact'
                    WHEN z.lat IS NOT NULL THEN 'zip_centroid'
                    ELSE 'none' END AS geo_method
        FROM keyed l
        LEFT JOIN stg_census_geocode g ON g.address_key = l.gkey AND g.match = 'Match'
        LEFT JOIN stg_census_zcta z ON z.zip = left(coalesce(l.zip, ''), 5);

        DELETE FROM practice_locations;
        INSERT INTO practice_locations
        SELECT practice_id, address_id, address1, address2, city, state, zip, phone, n_clinicians, loc_rank, lat, lon, geo_method
        FROM _loc_geo;

        CREATE OR REPLACE TEMP TABLE _ids AS
        SELECT practice_id, any_value(org_pac_id) AS org_pac_id, any_value(ind_pac_id) AS ind_pac_id,
               max(match_share) AS match_share
        FROM _practice_match GROUP BY 1;

        DELETE FROM practices;
        INSERT INTO practices
        WITH ids AS (SELECT * FROM _ids),
        latest AS (SELECT max(performance_year) AS y FROM stg_onc_clinician_cehrt),
        -- vendor over the union of the mapped groups' clinicians, latest performance year
        rows_ AS (
            SELECT m.practice_id, s.npi,
                   coalesce(vm.vendor, s.developer_name_raw) AS vendor,
                   CASE WHEN s.developer_name_raw IS NULL THEN NULL ELSE coalesce(vm.vendor_tier, 'unmapped') END AS vendor_tier
            FROM _practice_match m
            JOIN stg_onc_clinician_cehrt s USING (group_key, performance_year)
            LEFT JOIN ref_vendor_map vm ON lower(trim(vm.developer_name_raw)) = lower(trim(s.developer_name_raw))
            WHERE s.performance_year = (SELECT y FROM latest)
        ), grp AS (
            SELECT m.practice_id, count(*) AS n_groups,
                   string_agg(m.group_key, '|' ORDER BY v.n_clinicians DESC, m.group_key) AS group_keys,
                   sum(v.n_clinicians * coalesce(v.hospital_affiliated_share, 0)) / sum(v.n_clinicians) AS hospital_affiliated_share
            FROM _practice_match m JOIN practice_vendor v USING (group_key, performance_year)
            WHERE m.performance_year = (SELECT y FROM latest)
            GROUP BY 1
        ), mips AS (
            SELECT practice_id, count(DISTINCT npi) AS n_mips_clinicians,
                   count(DISTINCT npi) FILTER (WHERE vendor_tier = 'core_ehr') AS n_core
            FROM rows_ GROUP BY 1
        ), per_vendor AS (
            SELECT practice_id, vendor, count(DISTINCT npi) AS n FROM rows_ WHERE vendor_tier = 'core_ehr' GROUP BY 1, 2
        ), ranked AS (
            SELECT *, row_number() OVER (PARTITION BY practice_id ORDER BY n DESC, vendor) AS rn,
                   count(*) OVER (PARTITION BY practice_id) AS n_vendors,
                   string_agg(vendor, '|' ORDER BY n DESC, vendor) OVER (PARTITION BY practice_id) AS all_vendors
            FROM per_vendor
        ), top AS (
            SELECT r.practice_id, r.vendor, r.n, r.n_vendors, r.all_vendors,
                   (SELECT count(*) FROM per_vendor p WHERE p.practice_id = r.practice_id AND p.n = r.n) > 1 AS is_tie
            FROM ranked r WHERE rn = 1
        ), identity AS ({identity}),
        spec AS ({spec}),
        loc AS (
            SELECT practice_id, count(*) AS n_locations, count(DISTINCT state) AS n_states,
                   arg_min(address1, loc_rank) AS address1, arg_min(address2, loc_rank) AS address2,
                   arg_min(city, loc_rank) AS city, arg_min(state, loc_rank) AS state, arg_min(zip, loc_rank) AS zip,
                   arg_min(phone, loc_rank) AS phone,
                   arg_min(lat, loc_rank) AS lat, arg_min(lon, loc_rank) AS lon, arg_min(geo_method, loc_rank) AS geo_method
            FROM _loc_geo GROUP BY 1
        )
        SELECT i.practice_id, 'cms_dac', i.org_pac_id, i.ind_pac_id, d.name,
               d.n_members, d.n_clinicians,
               coalesce(mp.n_mips_clinicians, 0), coalesce(g.n_groups, 0), g.group_keys, i.match_share,
               l.address1, l.address2, l.city, l.state, l.zip, l.phone,
               coalesce(l.n_locations, 0), coalesce(l.n_states, 0),
               l.lat, l.lon, coalesce(l.geo_method, 'none'),
               sp.top_specialty, sp.specialty_mix,
               t.vendor,
               CASE WHEN t.vendor IS NOT NULL THEN 'mips' END AS vendor_source,
               CASE WHEN mp.n_core > 0 THEN t.n / mp.n_core END AS vendor_share,
               (SELECT y FROM latest) AS vendor_year,
               coalesce(t.n_vendors, 0), t.all_vendors, coalesce(t.is_tie, FALSE),
               g.hospital_affiliated_share,
               NULL, NULL, NULL, NULL,   -- lantern_* are filled by the lantern derive step (#19)
               d.source_file_id
        FROM ids i
        JOIN identity d USING (practice_id)
        LEFT JOIN grp g USING (practice_id)
        LEFT JOIN mips mp USING (practice_id)
        LEFT JOIN top t USING (practice_id)
        LEFT JOIN spec sp USING (practice_id)
        LEFT JOIN loc l USING (practice_id);

        DROP TABLE _loc_geo; DROP TABLE _practice_loc; DROP TABLE _practice_match; DROP TABLE _ids;
        "#,
        max_geo = MAX_GEOCODED_LOCATIONS,
        akey = key("l.address1", "l.city", "l.state", "l.zip"),
        identity = dac_identity_sql("_ids"),
        spec = dac_specialty_sql("_ids"),
    ))?;

    let n_p = count(conn, "SELECT count(*) FROM practices")?;
    let n_l = count(conn, "SELECT count(*) FROM practice_locations")?;
    let n_groups = count(conn, "SELECT count(*) FROM practice_vendor")?;
    let matched = count(conn, "SELECT count(*) FROM practice_vendor WHERE practice_id IS NOT NULL")?;
    let by_method: Vec<String> = conn
        .prepare("SELECT geo_method || ':' || count(*) FROM practices GROUP BY geo_method ORDER BY count(*) DESC")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    let orphan = count(conn, "SELECT count(*) FROM practice_vendor v WHERE practice_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM practices p WHERE p.practice_id = v.practice_id)")?;
    ensure!(orphan == 0, "{orphan} practice_vendor rows point at a practice_id missing from practices");
    let dup = count(conn, "SELECT count(*) - count(DISTINCT (practice_id, address_id)) FROM practice_locations")?;
    ensure!(dup == 0, "practice_locations has {dup} duplicate keys");
    info!(
        practices = n_p,
        practice_locations = n_l,
        groups_matched = format!("{matched}/{n_groups}"),
        geo_by_method = ?by_method,
        "practice tables derived"
    );
    Ok(())
}
