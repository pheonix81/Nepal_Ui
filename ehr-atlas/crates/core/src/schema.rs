//! Canonical DuckDB schema.
//!
//! Naming:
//! * `source_files`  – provenance manifest (one row per downloaded file)
//! * `ref_*`         – reviewable reference tables loaded from `config/*.csv`
//! * `stg_*`         – one staging table per source, typed but otherwise faithful to the file
//! * `hospitals`, `hospital_ehr`, `hospital_vendor_year` – core final tables
//! * `hospital_*`, `clinician_*`, `practice*`, `fhir_endpoints`, `chpl_products`, `health_systems`
//!                   – enrichment final tables, each derived from its `stg_*` by `build`
//! * `hospital_current_vendor`, `hospital_profile`, `hospital_vendor_history`,
//!   `hospital_vendor_changes` – views over the final tables
//!
//! Every enrichment source follows the same contract: a subcommand downloads the raw
//! file(s) and loads `stg_<source>`; `build` derives the final table(s) from staging
//! (skipping sources whose staging table is empty) and recreates the views.

/// Core tables that are (re)created on every `build`. Order matters (views last).
pub const DDL: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS source_files (
        id                TEXT PRIMARY KEY,
        source            TEXT NOT NULL,
        url               TEXT NOT NULL,
        path              TEXT NOT NULL,
        sha256            TEXT NOT NULL,
        bytes             BIGINT NOT NULL,
        downloaded_at     TIMESTAMP NOT NULL,
        upstream_modified TEXT,
        notes             TEXT
    )"#,
    r#"CREATE TABLE IF NOT EXISTS ref_vendor_map (
        developer_name_raw TEXT PRIMARY KEY,
        vendor             TEXT NOT NULL,
        vendor_tier        TEXT NOT NULL,   -- core_ehr | ancillary
        notes              TEXT
    )"#,
    r#"CREATE TABLE IF NOT EXISTS ref_core_products (
        vendor          TEXT NOT NULL,
        product_pattern TEXT NOT NULL,      -- SQL ILIKE pattern on product_name_raw
        notes           TEXT
    )"#,
    r#"CREATE TABLE IF NOT EXISTS ref_spot_checks (
        ccn             TEXT PRIMARY KEY,
        name            TEXT NOT NULL,
        expected_vendor TEXT NOT NULL,
        notes           TEXT
    )"#,
    // Hand-entered corrections for hospitals that switched vendor after their latest
    // attestation, or that are outside the PI program. They win over the attested vendor.
    r#"CREATE TABLE IF NOT EXISTS ref_vendor_overrides (
        ccn     TEXT PRIMARY KEY,
        name    TEXT,
        vendor  TEXT NOT NULL,              -- canonical vendor name
        as_of   DATE NOT NULL,              -- when the information was received
        source  TEXT NOT NULL,
        notes   TEXT
    )"#,
    r#"CREATE TABLE IF NOT EXISTS stg_cms_hospitals (
        ccn                 TEXT NOT NULL,
        name                TEXT,
        address             TEXT,
        city                TEXT,
        state               TEXT,
        zip                 TEXT,
        county              TEXT,
        phone               TEXT,
        hospital_type       TEXT,
        ownership           TEXT,
        emergency_services  BOOLEAN,
        birthing_friendly   BOOLEAN,
        overall_rating      SMALLINT,
        source_file_id      TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS stg_onc_linkage (
        ccn                  TEXT NOT NULL,
        facility_name        TEXT,
        address              TEXT,
        city                 TEXT,
        state                TEXT,
        zip                  TEXT,
        county               TEXT,
        phone                TEXT,
        meets_pi_criteria    BOOLEAN,
        period_start         DATE,
        period_end           DATE,
        year                 SMALLINT NOT NULL,
        cehrt_id_raw         TEXT,               -- exactly as reported
        cehrt_id             TEXT,               -- normalised: upper-cased, O→0 in the '0015C' prefix, NULL if not a plausible ID
        chpl_id              TEXT,
        chpl_product_db_id   TEXT,
        developer_name_raw   TEXT,
        product_name_raw     TEXT,
        source_file_id       TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS stg_cms_pi (
        ccn                  TEXT NOT NULL,
        facility_name        TEXT,
        address              TEXT,
        city                 TEXT,
        state                TEXT,
        zip                  TEXT,
        county               TEXT,
        phone                TEXT,
        meets_pi_criteria    BOOLEAN,
        period_start         DATE,
        period_end           DATE,
        year                 SMALLINT NOT NULL,
        cehrt_id_raw         TEXT,
        cehrt_id             TEXT,
        source_file_id       TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS stg_chpl_cehrt (
        cehrt_id             TEXT NOT NULL,
        chpl_id              TEXT,
        chpl_product_db_id   TEXT,
        developer_name_raw   TEXT,
        product_name_raw     TEXT,
        version_raw          TEXT,
        edition_year         TEXT,
        fetched_at           TIMESTAMP NOT NULL,
        status               TEXT NOT NULL      -- ok | not_found | error
    )"#,
    r#"CREATE TABLE IF NOT EXISTS hospitals (
        ccn                 TEXT PRIMARY KEY,
        name                TEXT NOT NULL,
        address             TEXT,
        city                TEXT,
        state               TEXT,
        zip                 TEXT,
        county              TEXT,
        phone               TEXT,
        hospital_type       TEXT,
        ownership           TEXT,
        emergency_services  BOOLEAN,
        birthing_friendly   BOOLEAN,
        overall_rating      SMALLINT,
        in_cms_hgi          BOOLEAN NOT NULL,   -- FALSE ⇒ only seen in PI/ONC data (closed/merged/renumbered)
        source_file_id      TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS hospital_ehr (
        ccn                 TEXT NOT NULL,
        year                SMALLINT NOT NULL,
        cehrt_id            TEXT NOT NULL,
        chpl_id             TEXT,               -- NULL when the CEHRT ID is unresolved
        chpl_product_db_id  TEXT,
        developer_name_raw  TEXT,
        product_name_raw    TEXT,
        vendor              TEXT,               -- canonical, from ref_vendor_map; NULL if unresolved
        vendor_tier         TEXT,               -- core_ehr | ancillary | unmapped
        is_core_product     BOOLEAN NOT NULL,
        attest_source       TEXT NOT NULL,      -- which file says the hospital used this CEHRT ID: onc_linkage | cms_pi
        resolve_source      TEXT,               -- which source expanded the CEHRT ID to products: onc_linkage | chpl | NULL (unresolved)
        source_file_id      TEXT NOT NULL       -- attestation file
        -- logical key (ccn, year, cehrt_id, chpl_id); chpl_id is NULL for unresolved IDs so no PK; uniqueness asserted in build
    )"#,
    r#"CREATE TABLE IF NOT EXISTS hospital_vendor_year (
        ccn              TEXT NOT NULL,
        year             SMALLINT NOT NULL,
        primary_vendor   TEXT,                  -- NULL when nothing resolvable
        primary_cehrt_id TEXT,
        n_modules        INTEGER NOT NULL,
        n_vendors        INTEGER NOT NULL,      -- distinct canonical vendors, any tier
        n_core_vendors   INTEGER NOT NULL,      -- distinct vendors with tier = core_ehr
        core_vendors     TEXT,                  -- '|'-joined, sorted
        all_vendors      TEXT,
        rule             TEXT NOT NULL,         -- which rule step decided (see README)
        is_tie           BOOLEAN NOT NULL,
        PRIMARY KEY (ccn, year)
    )"#,
];

// ===========================================================================
// Enrichment: staging tables (one per source) and the final tables `build` derives.
// Each block is owned by one source module; keep blocks self-contained so they can be
// edited independently.
// ===========================================================================
pub const ENRICH_DDL: &[&str] = &[
    // ---- cms_enrollments (#2) -------------------------------------------------------
    r#"CREATE TABLE IF NOT EXISTS stg_cms_enrollments (
        enrollment_id          TEXT NOT NULL,
        enrollment_state       TEXT,
        provider_type_code     TEXT,
        provider_type_text     TEXT,
        npi                    TEXT NOT NULL,
        multiple_npi_flag      BOOLEAN,
        ccn                    TEXT NOT NULL,
        associate_id           TEXT,
        organization_name      TEXT,
        dba_name               TEXT,
        incorporation_date     DATE,
        incorporation_state    TEXT,
        org_type_structure     TEXT,
        proprietary_nonprofit  TEXT,               -- P | N
        address1               TEXT,
        address2               TEXT,
        city                   TEXT,
        state                  TEXT,
        zip                    TEXT,
        practice_location_type TEXT,
        reh_conversion_flag    BOOLEAN,
        reh_conversion_date    DATE,
        cah_or_hospital_ccn    TEXT,               -- parent CCN for units / subproviders
        source_file_id         TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS hospital_npi (
        ccn            TEXT NOT NULL,
        npi            TEXT NOT NULL,
        npi_org_name   TEXT,
        dba_name       TEXT,
        enrollment_id  TEXT,
        match_method   TEXT NOT NULL,             -- cms_enrollments
        source_file_id TEXT,
        PRIMARY KEY (ccn, npi)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS hospital_enrollment (
        ccn                    TEXT PRIMARY KEY,   -- one row per CCN (the primary enrollment)
        enrollment_id          TEXT,
        organization_name      TEXT,
        dba_name               TEXT,
        incorporation_date     DATE,
        incorporation_state    TEXT,
        org_type_structure     TEXT,
        proprietary_nonprofit  TEXT,
        address1               TEXT,
        address2               TEXT,
        city                   TEXT,
        state                  TEXT,
        zip                    TEXT,
        reh_conversion_flag    BOOLEAN,
        reh_conversion_date    DATE,
        parent_ccn             TEXT,
        n_npis                 INTEGER,
        source_file_id         TEXT
    )"#,

    // ---- geocode (#3): census_zcta + census_geocode ---------------------------------
    r#"CREATE TABLE IF NOT EXISTS stg_census_zcta (
        zip            TEXT PRIMARY KEY,
        lat            DOUBLE NOT NULL,
        lon            DOUBLE NOT NULL,
        source_file_id TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS stg_census_geocode (
        address_key     TEXT PRIMARY KEY,         -- sha256 prefix of the normalised input address
        input_address   TEXT NOT NULL,
        match           TEXT NOT NULL,            -- Match | Tie | No_Match
        match_type      TEXT,                     -- Exact | Non_Exact
        matched_address TEXT,
        lat             DOUBLE,
        lon             DOUBLE,
        tiger_line_id   TEXT,
        side            TEXT,
        geocoded_at     TIMESTAMP NOT NULL,
        source_file_id  TEXT
    )"#,
    r#"CREATE TABLE IF NOT EXISTS hospital_geo (
        ccn             TEXT PRIMARY KEY,
        lat             DOUBLE,
        lon             DOUBLE,
        method          TEXT NOT NULL,            -- census_exact | census_nonexact | zip_centroid | none
        address_source  TEXT,                     -- cms_hospitals | cms_enrollments
        input_address   TEXT,
        matched_address TEXT
    )"#,

    // ---- hcris (#4) ----------------------------------------------------------------
    r#"CREATE TABLE IF NOT EXISTS stg_hcris (
        ccn                       TEXT NOT NULL,
        fiscal_year               SMALLINT NOT NULL,   -- from the file's year
        fy_begin                  DATE,
        fy_end                    DATE,
        city                      TEXT,
        state                     TEXT,
        facility_type             TEXT,               -- "CCN Facility Type"
        fte_employees             DOUBLE,
        interns_residents_fte     DOUBLE,
        beds                      INTEGER,
        bed_days_available        BIGINT,
        discharges_total          INTEGER,
        discharges_medicare       INTEGER,
        discharges_medicaid       INTEGER,
        inpatient_revenue         DOUBLE,
        outpatient_revenue        DOUBLE,
        total_patient_revenue     DOUBLE,
        net_patient_revenue       DOUBLE,
        total_costs               DOUBLE,
        cost_to_charge_ratio      DOUBLE,
        charity_care_cost         DOUBLE,
        uncompensated_care_cost   DOUBLE,
        medicaid_net_revenue      DOUBLE,
        source_file_id            TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS hospital_cost_reports (
        ccn                       TEXT NOT NULL,
        fiscal_year               SMALLINT NOT NULL,
        fy_begin                  DATE,
        fy_end                    DATE,
        beds                      INTEGER,
        bed_days_available        BIGINT,
        discharges_total          INTEGER,
        discharges_medicare       INTEGER,
        discharges_medicaid       INTEGER,
        fte_employees             DOUBLE,
        interns_residents_fte     DOUBLE,
        inpatient_revenue         DOUBLE,
        outpatient_revenue        DOUBLE,
        total_patient_revenue     DOUBLE,
        net_patient_revenue       DOUBLE,
        total_costs               DOUBLE,
        cost_to_charge_ratio      DOUBLE,
        charity_care_cost         DOUBLE,
        uncompensated_care_cost   DOUBLE,
        medicaid_net_revenue      DOUBLE,
        source_file_id            TEXT,
        PRIMARY KEY (ccn, fiscal_year)
    )"#,

    // ---- cms_pos (#5) --------------------------------------------------------------
    r#"CREATE TABLE IF NOT EXISTS stg_cms_pos (
        ccn                      TEXT NOT NULL,
        quarter                  TEXT NOT NULL,       -- e.g. 2026Q2, from the file name
        category_code            TEXT,                -- PRVDR_CTGRY_CD ('01' = hospital)
        subtype_code             TEXT,                -- PRVDR_CTGRY_SBTYP_CD
        name                     TEXT,
        address                  TEXT,
        city                     TEXT,
        state                    TEXT,
        zip                      TEXT,
        fips_state               TEXT,
        fips_county              TEXT,
        cbsa_code                TEXT,
        cbsa_urban_rural         TEXT,                -- U | R
        general_control_type     TEXT,                -- GNRL_CNTL_TYPE_CD
        certification_date       DATE,
        original_participation   DATE,
        termination_code         TEXT,                -- PGM_TRMNTN_CD ('00' = active)
        termination_date         DATE,
        accreditation_type       TEXT,
        accreditation_effective  DATE,
        accreditation_expiration DATE,
        bed_count                INTEGER,
        certified_bed_count      INTEGER,
        medical_school_affiliation TEXT,              -- MDCL_SCHL_AFLTN_CD
        intern_resident_service  TEXT,                -- INTRN_RSDNT_SRVC_CD
        multi_facility_org       TEXT,                -- MLT_FAC_ORG_NAME
        chow_count               INTEGER,
        chow_date                DATE,
        cross_ref_ccn            TEXT,
        source_file_id           TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS hospital_pos (
        ccn                      TEXT PRIMARY KEY,   -- latest quarter per CCN
        quarter                  TEXT NOT NULL,
        subtype_code             TEXT,
        name                     TEXT,
        cbsa_code                TEXT,
        cbsa_urban_rural         TEXT,
        general_control_type     TEXT,
        certification_date       DATE,
        original_participation   DATE,
        termination_code         TEXT,
        termination_date         DATE,
        is_active                BOOLEAN,
        accreditation_type       TEXT,
        accreditation_effective  DATE,
        bed_count                INTEGER,
        certified_bed_count      INTEGER,
        medical_school_affiliation TEXT,
        intern_resident_service  TEXT,
        multi_facility_org       TEXT,
        chow_count               INTEGER,
        chow_date                DATE,
        cross_ref_ccn            TEXT,
        source_file_id           TEXT
    )"#,

    // ---- cms_measures (#6) ---------------------------------------------------------
    r#"CREATE TABLE IF NOT EXISTS ref_measures (
        dataset              TEXT NOT NULL,          -- short name from CMS_MEASURE_DATASETS
        dataset_id           TEXT NOT NULL,
        mode                 TEXT NOT NULL,          -- long (one measure per row) | wide (one row per hospital, one config row per score column)
        ccn_column           TEXT NOT NULL,
        measure_id_columns   TEXT,                   -- '|'-separated; several are joined with ' / '
        measure_id_literal   TEXT,                   -- wide mode: the measure id for this value column
        measure_name_column  TEXT,
        measure_name_literal TEXT,
        value_columns        TEXT NOT NULL,          -- '|'-separated; first non-empty wins
        denominator_column   TEXT,
        compared_column      TEXT,
        footnote_column      TEXT,
        start_column         TEXT,
        end_column           TEXT,
        notes                TEXT
    )"#,
    r#"CREATE TABLE IF NOT EXISTS stg_cms_measures (
        ccn                  TEXT NOT NULL,
        dataset              TEXT NOT NULL,
        measure_id           TEXT NOT NULL,
        measure_name         TEXT,
        value_raw            TEXT,
        value                DOUBLE,                  -- NULL when "Not Available" etc.
        denominator          TEXT,
        compared_to_national TEXT,
        footnote             TEXT,
        start_date           DATE,
        end_date             DATE,
        source_file_id       TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS hospital_measures (
        ccn                  TEXT NOT NULL,
        dataset              TEXT NOT NULL,
        measure_id           TEXT NOT NULL,
        measure_name         TEXT,
        value_raw            TEXT,
        value                DOUBLE,
        denominator          TEXT,
        compared_to_national TEXT,
        footnote             TEXT,
        start_date           DATE,
        end_date             DATE,
        source_file_id       TEXT,
        PRIMARY KEY (ccn, dataset, measure_id)
    )"#,

    // ---- cms_inpatient (#7) --------------------------------------------------------
    r#"CREATE TABLE IF NOT EXISTS stg_cms_inpatient (
        ccn                    TEXT NOT NULL,
        year                   SMALLINT NOT NULL,
        org_name               TEXT,
        city                   TEXT,
        state                  TEXT,
        zip                    TEXT,
        ruca                   TEXT,
        ruca_desc              TEXT,
        tot_benes              INTEGER,
        tot_dschrgs            INTEGER,
        tot_cvrd_days          INTEGER,
        tot_days               INTEGER,
        tot_submtd_cvrd_chrg   DOUBLE,
        tot_pymt_amt           DOUBLE,
        tot_mdcr_pymt_amt      DOUBLE,
        bene_avg_age           DOUBLE,
        bene_age_lt_65         INTEGER,
        bene_age_65_74         INTEGER,
        bene_age_75_84         INTEGER,
        bene_age_gt_84         INTEGER,
        bene_female            INTEGER,
        bene_male              INTEGER,
        bene_race_white        INTEGER,
        bene_race_black        INTEGER,
        bene_race_api          INTEGER,
        bene_race_hispanic     INTEGER,
        bene_race_natind       INTEGER,
        bene_race_other        INTEGER,
        bene_dual              INTEGER,
        source_file_id         TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS hospital_medicare_inpatient (
        ccn                    TEXT NOT NULL,
        year                   SMALLINT NOT NULL,
        ruca                   TEXT,
        ruca_desc              TEXT,
        tot_benes              INTEGER,
        tot_dschrgs            INTEGER,
        tot_cvrd_days          INTEGER,
        tot_days               INTEGER,
        tot_submtd_cvrd_chrg   DOUBLE,
        tot_pymt_amt           DOUBLE,
        tot_mdcr_pymt_amt      DOUBLE,
        bene_avg_age           DOUBLE,
        bene_age_lt_65         INTEGER,
        bene_age_65_74         INTEGER,
        bene_age_75_84         INTEGER,
        bene_age_gt_84         INTEGER,
        bene_female            INTEGER,
        bene_male              INTEGER,
        bene_race_white        INTEGER,
        bene_race_black        INTEGER,
        bene_race_api          INTEGER,
        bene_race_hispanic     INTEGER,
        bene_race_natind       INTEGER,
        bene_race_other        INTEGER,
        bene_dual              INTEGER,
        source_file_id         TEXT,
        PRIMARY KEY (ccn, year)
    )"#,

    // ---- cms_service_area (#8) -----------------------------------------------------
    r#"CREATE TABLE IF NOT EXISTS stg_cms_service_area (
        ccn            TEXT NOT NULL,
        year           SMALLINT NOT NULL,
        zip            TEXT NOT NULL,
        total_days     INTEGER,                      -- NULL when suppressed ('*')
        total_charges  DOUBLE,
        total_cases    INTEGER,
        source_file_id TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS hospital_service_area (
        ccn            TEXT NOT NULL,
        year           SMALLINT NOT NULL,
        zip            TEXT NOT NULL,
        total_days     INTEGER,
        total_charges  DOUBLE,
        total_cases    INTEGER,
        source_file_id TEXT,
        PRIMARY KEY (ccn, year, zip)
    )"#,

    // ---- ahrq_compendium (#9) ------------------------------------------------------
    r#"CREATE TABLE IF NOT EXISTS stg_ahrq_linkage (
        compendium_year        SMALLINT NOT NULL,
        compendium_hospital_id TEXT,
        ccn                    TEXT,                 -- NULL for the ~2% of rows AHRQ could not assign a CCN
        hospital_name          TEXT,
        hospital_street        TEXT,
        hospital_city          TEXT,
        hospital_state         TEXT,
        hospital_zip           TEXT,
        acute_hospital         BOOLEAN,
        health_sys_id          TEXT,                 -- NULL ⇒ independent (not in any Compendium system)
        health_sys_name        TEXT,
        health_sys_city        TEXT,
        health_sys_state       TEXT,
        corp_parent_id         TEXT,                 -- 2023+: corporate parent (IDN etc.)
        corp_parent_name       TEXT,
        corp_parent_type       TEXT,
        hos_beds               INTEGER,
        hos_discharges         INTEGER,
        hos_residents          DOUBLE,
        hos_major_teaching     BOOLEAN,
        hos_ownership_code     TEXT,
        hos_net_revenue        DOUBLE,
        hos_total_revenue      DOUBLE,
        extra                  JSON,                 -- every other column, for later use
        source_file_id         TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS stg_ahrq_systems (
        compendium_year   SMALLINT NOT NULL,
        health_sys_id     TEXT NOT NULL,
        health_sys_name   TEXT,
        health_sys_city   TEXT,
        health_sys_state  TEXT,
        aha_sysid         TEXT,
        total_mds         INTEGER,
        n_groups          INTEGER,
        n_hospitals       INTEGER,
        n_acute_hospitals INTEGER,
        n_beds            INTEGER,
        n_discharges      INTEGER,
        multistate_code   TEXT,                      -- 1 single state | 2 two states | 3 three+ states
        any_insurance_product BOOLEAN,
        ownership_code    TEXT,
        net_revenue       DOUBLE,
        total_revenue     DOUBLE,
        extra             JSON,
        source_file_id    TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS hospital_systems (
        ccn                    TEXT NOT NULL,
        compendium_year        SMALLINT NOT NULL,
        health_sys_id          TEXT NOT NULL,        -- only hospitals that belong to a system
        health_sys_name        TEXT,
        health_sys_state       TEXT,
        compendium_hospital_id TEXT,
        corp_parent_id         TEXT,
        corp_parent_name       TEXT,
        corp_parent_type       TEXT,
        hos_beds               INTEGER,
        hos_discharges         INTEGER,
        hos_major_teaching     BOOLEAN,
        source_file_id         TEXT,
        PRIMARY KEY (ccn, compendium_year)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS health_systems (
        health_sys_id     TEXT NOT NULL,
        compendium_year   SMALLINT NOT NULL,
        health_sys_name   TEXT,
        health_sys_city   TEXT,
        health_sys_state  TEXT,
        aha_sysid         TEXT,
        total_mds         INTEGER,
        n_groups          INTEGER,
        n_hospitals       INTEGER,
        n_acute_hospitals INTEGER,
        n_beds            INTEGER,
        n_discharges      INTEGER,
        multistate        BOOLEAN,                   -- operates in more than one state
        multistate_code   TEXT,
        any_insurance_product BOOLEAN,
        ownership_code    TEXT,
        net_revenue       DOUBLE,
        total_revenue     DOUBLE,
        source_file_id    TEXT,
        PRIMARY KEY (health_sys_id, compendium_year)
    )"#,

    // ---- clinicians (#10): cms_facility_affiliation + onc_clinician_cehrt -----------
    r#"CREATE TABLE IF NOT EXISTS stg_cms_facility_affiliation (
        npi            TEXT NOT NULL,
        pac_id         TEXT,
        last_name      TEXT,
        first_name     TEXT,
        facility_type  TEXT,
        ccn            TEXT NOT NULL,
        source_file_id TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS stg_onc_clinician_cehrt (
        performance_year   SMALLINT NOT NULL,
        provider_key       TEXT,
        npi                TEXT NOT NULL,
        group_key          TEXT,
        practice_state     TEXT,
        practice_size      INTEGER,
        specialty          TEXT,
        participation_type TEXT,
        chpl_id            TEXT,
        developer_name_raw TEXT,
        product_name_raw   TEXT,
        chpl_product_db_id TEXT,
        edition            TEXT,
        source_file_id     TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS clinician_affiliations (
        npi            TEXT NOT NULL,
        ccn            TEXT NOT NULL,
        facility_type  TEXT,
        source_file_id TEXT,
        PRIMARY KEY (npi, ccn)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS clinician_cehrt (
        npi                TEXT NOT NULL,
        performance_year   SMALLINT NOT NULL,
        chpl_id            TEXT NOT NULL,
        developer_name_raw TEXT,
        product_name_raw   TEXT,
        vendor             TEXT,                     -- canonical via ref_vendor_map
        vendor_tier        TEXT,
        practice_state     TEXT,
        specialty          TEXT,
        source_file_id     TEXT,
        PRIMARY KEY (npi, performance_year, chpl_id)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS hospital_clinician_vendor (
        ccn               TEXT NOT NULL,
        performance_year  SMALLINT NOT NULL,
        vendor            TEXT NOT NULL,
        n_clinicians      INTEGER NOT NULL,
        share             DOUBLE NOT NULL,           -- of affiliated clinicians with a core-tier vendor
        rank              INTEGER NOT NULL,
        PRIMARY KEY (ccn, performance_year, vendor)
    )"#,

    // One row per MIPS reporting entity (practice) × performance year: the majority
    // core-tier vendor among its clinicians. The group key is ONC's opaque id; `practice_id`
    // is filled by the cms_dac derive step (#18) once the group's clinicians are matched to a
    // Doctors & Clinicians organisation.
    r#"CREATE TABLE IF NOT EXISTS practice_vendor (
        group_key             TEXT NOT NULL,
        performance_year      SMALLINT NOT NULL,
        participation_type    TEXT,                  -- Group | Individual | APM Entity | Subgroup
        practice_state        TEXT,
        practice_size         INTEGER,               -- as reported in the PUF
        n_clinicians          INTEGER NOT NULL,      -- distinct NPIs in the file
        n_core_clinicians     INTEGER NOT NULL,      -- distinct NPIs with any core-tier vendor
        vendor                TEXT,                  -- majority core-tier vendor; NULL if none
        vendor_share          DOUBLE,                -- of n_core_clinicians
        n_vendors             INTEGER NOT NULL,      -- distinct core-tier vendors
        all_vendors           TEXT,                  -- '|'-joined, by clinician count desc
        is_tie                BOOLEAN NOT NULL,
        top_specialty         TEXT,
        hospital_affiliated_share DOUBLE,            -- share of clinicians with any hospital affiliation
        practice_id           TEXT,                  -- practices.practice_id; NULL when no D&C match (#18)
        match_method          TEXT,                  -- org (group PAC ID by clinician overlap) | individual (solo clinician's own enrollment)
        match_share           DOUBLE,                -- share of the group's clinicians found under that organisation
        PRIMARY KEY (group_key, performance_year)
    )"#,

    // ---- cms_dac (#18): Doctors & Clinicians National Downloadable File --------------
    r#"CREATE TABLE IF NOT EXISTS stg_cms_dac (
        npi                   TEXT NOT NULL,
        ind_pac_id            TEXT,
        ind_enrl_id           TEXT,
        last_name             TEXT,
        first_name            TEXT,
        middle_name           TEXT,
        suffix                TEXT,
        gender                TEXT,
        credential            TEXT,
        med_school            TEXT,
        grad_year             SMALLINT,
        primary_specialty     TEXT,
        secondary_specialties TEXT,                  -- sec_spec_all, '|'-separated as published
        telehealth            BOOLEAN,
        org_name              TEXT,                  -- "Facility Name": the group's legal business name
        org_pac_id            TEXT,                  -- NULL for clinicians enrolled without a group
        org_members           INTEGER,               -- num_org_mem
        address1              TEXT,
        address2              TEXT,
        address2_suppressed   BOOLEAN,               -- ln_2_sprs: line 2 is not a usable address line
        city                  TEXT,
        state                 TEXT,
        zip                   TEXT,                  -- ZIP or ZIP+4 without hyphen, as published
        phone                 TEXT,
        ind_assignment        TEXT,                  -- ind_assgn: Y | M | N
        group_assignment      TEXT,                  -- grp_assgn
        address_id            TEXT,                  -- adrs_id: CMS's key for the practice location
        source_file_id        TEXT NOT NULL
    )"#,
    // One row per practice. `source = cms_dac`: a Doctors & Clinicians organisation
    // (`org:<org_pac_id>`) that at least one MIPS group in practice_vendor maps to, or a solo
    // clinician enrolled without a group (`ind:<ind_pac_id>`); vendor fields are recomputed over
    // the clinicians of every MIPS group that maps to the practice, latest performance year.
    // `source = lantern` (#19): an organisation from a vendor's FHIR endpoint list that matched
    // no MIPS practice — identified in D&C by name (`org:` / `ind:` id, D&C attributes) or not
    // (`lantern:<hash>`, name and ZIP centroid from the list only); vendor is the list's
    // developer, lower confidence.
    r#"CREATE TABLE IF NOT EXISTS practices (
        practice_id           TEXT PRIMARY KEY,
        source                TEXT NOT NULL,         -- cms_dac | lantern
        org_pac_id            TEXT,
        ind_pac_id            TEXT,
        name                  TEXT NOT NULL,
        n_members             INTEGER,               -- group size as published (num_org_mem)
        n_clinicians          INTEGER,               -- distinct NPIs under the organisation in D&C; NULL when not identified there
        n_mips_clinicians     INTEGER NOT NULL,      -- distinct NPIs across the mapped MIPS groups
        n_groups              INTEGER NOT NULL,      -- MIPS groups (practice_vendor rows) mapped here
        group_keys            TEXT,                  -- '|'-joined
        match_share           DOUBLE,                -- best group's match share
        address1              TEXT,                  -- primary location: the address with the most clinicians
        address2              TEXT,
        city                  TEXT,
        state                 TEXT,
        zip                   TEXT,
        phone                 TEXT,
        n_locations           INTEGER NOT NULL,
        n_states              INTEGER NOT NULL,
        lat                   DOUBLE,
        lon                   DOUBLE,
        geo_method            TEXT NOT NULL,         -- census_exact | census_nonexact | zip_centroid | none
        top_specialty         TEXT,
        specialty_mix         TEXT,                  -- top five "specialty:clinicians", '|'-joined
        vendor                TEXT,                  -- majority core-tier vendor; NULL if none
        vendor_source         TEXT,                  -- mips (attested by the practice's PI reporters) | lantern (endpoint list only)
        vendor_share          DOUBLE,
        vendor_year           SMALLINT,
        n_vendors             INTEGER NOT NULL,
        all_vendors           TEXT,
        is_tie                BOOLEAN NOT NULL,
        hospital_affiliated_share DOUBLE,
        lantern_vendor        TEXT,                  -- developer of the FHIR endpoint list(s) naming this practice (#19); NULL when none
        lantern_n_endpoints   INTEGER,               -- endpoints matched to the practice
        lantern_match_method  TEXT,                  -- npi | name_zip | name_state | name | fuzzy_name_state; NULL = not matched (lantern:) or no Lantern row (cms_dac)
        lantern_match_score   DOUBLE,
        source_file_id        TEXT
    )"#,
    r#"CREATE TABLE IF NOT EXISTS practice_locations (
        practice_id           TEXT NOT NULL,
        address_id            TEXT NOT NULL,
        address1              TEXT,
        address2              TEXT,
        city                  TEXT,
        state                 TEXT,
        zip                   TEXT,
        phone                 TEXT,
        n_clinicians          INTEGER NOT NULL,      -- distinct NPIs of the organisation at this address
        loc_rank              INTEGER NOT NULL,      -- 1 = primary (most clinicians)
        lat                   DOUBLE,
        lon                   DOUBLE,
        geo_method            TEXT NOT NULL,
        PRIMARY KEY (practice_id, address_id)
    )"#,

    // ---- lantern (#11) -------------------------------------------------------------
    r#"CREATE TABLE IF NOT EXISTS stg_lantern_endpoints (
        url                     TEXT NOT NULL,
        list_source             TEXT,                -- the vendor-published endpoint list this came from
        api_developer_name      TEXT,
        capability_fhir_version TEXT,
        format                  TEXT,
        http_response           INTEGER,
        smart_http_response     INTEGER,
        kind                    TEXT,
        cap_stat_exists         BOOLEAN,
        source                  TEXT,                -- CHPL | Other
        created_at              TIMESTAMP,
        updated_at              TIMESTAMP,
        source_file_id          TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS stg_lantern_orgs (
        url            TEXT NOT NULL,                 -- endpoint URL
        org_name       TEXT NOT NULL,                 -- from the vendor list bundle (Organization resource)
        org_identifier TEXT,                          -- NPI or other id if the bundle carries one
        address        TEXT,
        city           TEXT,
        state          TEXT,
        zip            TEXT,
        list_source    TEXT,
        source_file_id TEXT
    )"#,
    r#"CREATE TABLE IF NOT EXISTS fhir_endpoints (
        endpoint_url       TEXT NOT NULL,
        org_name           TEXT NOT NULL,
        vendor             TEXT,                      -- canonical via ref_vendor_map
        developer_name_raw TEXT,
        fhir_version       TEXT,
        http_response      INTEGER,
        ccn                TEXT,                      -- NULL when the organization is not a hospital we know
        match_method       TEXT,                      -- npi | exact_name | exact_enrollment_name | fuzzy_name_state | fuzzy_name | NULL
        match_score        DOUBLE,
        practice_id        TEXT,                      -- practices.practice_id (#19); NULL for hospitals and non-core vendors
        practice_match_method TEXT,                   -- npi | name_zip | name_state | name | fuzzy_name_state | NULL (a Lantern-only practice)
        practice_match_score  DOUBLE,
        observed_at        DATE,
        source_file_id     TEXT,
        PRIMARY KEY (endpoint_url, org_name)
    )"#,

    // ---- chpl_products (#12) -------------------------------------------------------
    r#"CREATE TABLE IF NOT EXISTS stg_chpl_products (
        chpl_id                 TEXT NOT NULL,        -- chplProductNumber
        chpl_product_db_id      TEXT,                 -- numeric id
        developer_name_raw      TEXT,
        product_name_raw        TEXT,
        version_raw             TEXT,
        edition                 TEXT,
        certification_date      DATE,
        certification_status    TEXT,
        certification_body      TEXT,
        criteria_met            TEXT,                 -- '|'-joined criteria numbers
        has_g10_fhir_api        BOOLEAN,
        cqm_count               INTEGER,
        surveillance_count      INTEGER,
        open_nonconformity_count INTEGER,
        closed_nonconformity_count INTEGER,
        decertification_date    DATE,
        fetched_at              TIMESTAMP NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS chpl_products (
        chpl_id                 TEXT PRIMARY KEY,
        chpl_product_db_id      TEXT,
        developer_name_raw      TEXT,
        vendor                  TEXT,                 -- canonical via ref_vendor_map
        product_name_raw        TEXT,
        version_raw             TEXT,
        edition                 TEXT,
        certification_date      DATE,
        certification_status    TEXT,
        certification_body      TEXT,
        criteria_met            TEXT,
        has_g10_fhir_api        BOOLEAN,
        cqm_count               INTEGER,
        surveillance_count      INTEGER,
        open_nonconformity_count INTEGER,
        closed_nonconformity_count INTEGER,
        decertification_date    DATE,
        fetched_at              TIMESTAMP
    )"#,

    // ---- mu_attestation (#29) ------------------------------------------------------
    // Meaningful Use attestations, 2011–2016: the hospital-years before the PI-era
    // `hospital_ehr` (2023+). Hospital rows only; eligible professionals stay in the raw file.
    r#"CREATE TABLE IF NOT EXISTS stg_mu_attestation (
        ccn                    TEXT NOT NULL,
        npi                    TEXT,
        state                  TEXT,                 -- Business_State_Territory (full name)
        zip                    TEXT,
        hospital_type          TEXT,                 -- General | Critical Access
        program_type           TEXT,                 -- Medicare | Medicare/Medicaid
        program_year           SMALLINT NOT NULL,
        stage                  TEXT,
        payment_year           SMALLINT,
        attestation_year       SMALLINT,
        attestation_month      SMALLINT,
        cehrt_id               TEXT NOT NULL,        -- EHR_Certification_Number (2011–2014 edition CEHRT ID)
        chp_id                 TEXT NOT NULL,        -- EHR_Product_CHP_Id (legacy CHPL product id)
        developer_name_raw     TEXT,                 -- Vendor_Name
        product_name_raw       TEXT,
        product_version        TEXT,
        product_classification TEXT,                 -- Complete EHR | Modular EHR
        product_setting        TEXT,                 -- Inpatient | Ambulatory
        edition                TEXT,
        source_file_id         TEXT NOT NULL
    )"#,
    // Same shape and meaning as `hospital_ehr`, for the MU years; CCNs limited to `hospitals`.
    r#"CREATE TABLE IF NOT EXISTS hospital_ehr_mu (
        ccn                    TEXT NOT NULL,
        year                   SMALLINT NOT NULL,    -- program year
        cehrt_id               TEXT NOT NULL,
        chp_id                 TEXT NOT NULL,
        developer_name_raw     TEXT,
        product_name_raw       TEXT,
        product_classification TEXT,
        vendor                 TEXT,                 -- canonical via ref_vendor_map (Paragon → Altera by product)
        vendor_tier            TEXT,                 -- core_ehr | ancillary | unmapped
        is_core_product        BOOLEAN NOT NULL,
        source_file_id         TEXT NOT NULL,
        PRIMARY KEY (ccn, year, cehrt_id, chp_id)
    )"#,
    // Same columns as `hospital_vendor_year`, same primary-vendor rule, MU years.
    r#"CREATE TABLE IF NOT EXISTS hospital_vendor_year_mu (
        ccn              TEXT NOT NULL,
        year             SMALLINT NOT NULL,
        primary_vendor   TEXT,
        primary_cehrt_id TEXT,
        n_modules        INTEGER NOT NULL,
        n_vendors        INTEGER NOT NULL,
        n_core_vendors   INTEGER NOT NULL,
        core_vendors     TEXT,
        all_vendors      TEXT,
        rule             TEXT NOT NULL,
        is_tie           BOOLEAN NOT NULL,
        PRIMARY KEY (ccn, year)
    )"#,
];

pub const VIEW_DDL: &[&str] = &[
    r#"CREATE OR REPLACE VIEW hospital_current_vendor AS
        WITH latest AS (
            SELECT ccn, MAX(year) AS year
            FROM hospital_vendor_year
            WHERE primary_vendor IS NOT NULL
            GROUP BY ccn
        )
        -- A row in ref_vendor_overrides replaces the attested vendor (rule = 'override');
        -- the attestation stays in attested_vendor / attested_year.
        SELECT h.ccn, h.name, h.city, h.state, h.zip, h.hospital_type, h.ownership,
               h.emergency_services, h.in_cms_hgi,
               CASE WHEN o.ccn IS NULL THEN v.year ELSE year(o.as_of)::SMALLINT END AS vendor_year,
               coalesce(o.vendor, v.primary_vendor) AS vendor,
               v.primary_cehrt_id AS cehrt_id,
               v.n_core_vendors, v.core_vendors, v.all_vendors,
               CASE WHEN o.ccn IS NULL THEN v.rule ELSE 'override' END AS rule,
               CASE WHEN o.ccn IS NULL THEN v.is_tie ELSE FALSE END AS is_tie,
               v.primary_vendor  AS attested_vendor,
               v.year            AS attested_year,
               o.as_of           AS override_as_of,
               o.source          AS override_source,
               o.notes           AS override_notes
        FROM hospitals h
        LEFT JOIN latest l USING (ccn)
        LEFT JOIN hospital_vendor_year v ON v.ccn = l.ccn AND v.year = l.year
        LEFT JOIN ref_vendor_overrides o ON o.ccn = h.ccn"#,
    // One row per CCN with the headline attribute from every enrichment source.
    r#"CREATE OR REPLACE VIEW hospital_profile AS
        WITH cr AS (
            SELECT * FROM hospital_cost_reports
            QUALIFY row_number() OVER (PARTITION BY ccn ORDER BY fiscal_year DESC) = 1
        ), mi AS (
            SELECT * FROM hospital_medicare_inpatient
            QUALIFY row_number() OVER (PARTITION BY ccn ORDER BY year DESC) = 1
        ), sys AS (
            SELECT * FROM hospital_systems
            QUALIFY row_number() OVER (PARTITION BY ccn ORDER BY compendium_year DESC) = 1
        ), cv AS (
            SELECT ccn, performance_year, vendor AS clinician_vendor, share AS clinician_vendor_share
            FROM hospital_clinician_vendor WHERE rank = 1
            QUALIFY row_number() OVER (PARTITION BY ccn ORDER BY performance_year DESC) = 1
        ), fe AS (
            SELECT ccn, count(*) AS n_fhir_endpoints,
                   arg_max(vendor, observed_at) AS fhir_vendor,
                   max(observed_at) AS fhir_observed_at
            FROM fhir_endpoints WHERE ccn IS NOT NULL GROUP BY ccn
        ), star AS (
            SELECT ccn,
                   max(value) FILTER (WHERE dataset = 'hcahps' AND measure_id = 'H_STAR_RATING') AS hcahps_star,
                   max(value) FILTER (WHERE dataset = 'complications_deaths' AND measure_id = 'Hybrid_HWM') AS hospital_wide_mortality_rate,
                   max(value) FILTER (WHERE dataset = 'mspb' AND measure_id = 'MSPB-1') AS mspb
            FROM hospital_measures GROUP BY ccn
        )
        SELECT v.*,
               g.lat, g.lon, g.method AS geo_method,
               e.organization_name AS legal_name, e.dba_name, e.proprietary_nonprofit, e.parent_ccn, e.n_npis,
               cr.fiscal_year AS cost_report_fy, cr.beds, cr.discharges_total, cr.net_patient_revenue, cr.total_costs,
               cr.fte_employees, cr.interns_residents_fte,
               CASE WHEN cr.beds IS NULL THEN NULL
                    WHEN cr.beds < 25 THEN '<25' WHEN cr.beds < 100 THEN '25-99'
                    WHEN cr.beds < 250 THEN '100-249' WHEN cr.beds < 500 THEN '250-499' ELSE '500+' END AS bed_band,
               coalesce(cr.interns_residents_fte, 0) > 0 AS is_teaching,
               p.cbsa_urban_rural, p.certified_bed_count, p.accreditation_type, p.is_active AS pos_active,
               p.termination_date, p.medical_school_affiliation,
               mi.year AS medicare_year, mi.ruca, mi.ruca_desc, mi.tot_dschrgs AS medicare_discharges, mi.tot_mdcr_pymt_amt AS medicare_payments,
               sys.health_sys_id, sys.health_sys_name, sys.compendium_year,
               cv.clinician_vendor, cv.clinician_vendor_share,
               fe.n_fhir_endpoints, fe.fhir_vendor, fe.fhir_observed_at,
               star.hcahps_star, star.hospital_wide_mortality_rate, star.mspb,
               (SELECT overall_rating FROM hospitals h2 WHERE h2.ccn = v.ccn) AS overall_rating
        FROM hospital_current_vendor v
        LEFT JOIN hospital_geo g USING (ccn)
        LEFT JOIN hospital_enrollment e USING (ccn)
        LEFT JOIN cr USING (ccn)
        LEFT JOIN hospital_pos p USING (ccn)
        LEFT JOIN mi USING (ccn)
        LEFT JOIN sys USING (ccn)
        LEFT JOIN cv USING (ccn)
        LEFT JOIN fe USING (ccn)
        LEFT JOIN star USING (ccn)"#,
    // Every attested hospital-year, both programs: Meaningful Use (era 'mu', 2011–2016)
    // and Promoting Interoperability (era 'pi', 2023+). No hospital-level source covers
    // 2017–2022. Overrides are not applied: this is what hospitals attested.
    r#"CREATE OR REPLACE VIEW hospital_vendor_history AS
        SELECT ccn, year, 'mu' AS era, primary_vendor, core_vendors, all_vendors, rule, is_tie
        FROM hospital_vendor_year_mu
        UNION ALL
        SELECT ccn, year, 'pi', primary_vendor, core_vendors, all_vendors, rule, is_tie
        FROM hospital_vendor_year"#,
    // One row per change of primary vendor between consecutive attested years of a CCN.
    // `across_gap` marks changes between years that are not adjacent (mostly 2016 → 2023):
    // the switch happened somewhere in between. `*_rule` say how firm each side is
    // ('core_product' / 'single_vendor' firm; 'module_count' / 'alphabetical_tie' weaker).
    r#"CREATE OR REPLACE VIEW hospital_vendor_changes AS
        WITH h AS (
            SELECT ccn, year, era, primary_vendor, rule,
                   lag(year)           OVER w AS prev_year,
                   lag(primary_vendor) OVER w AS prev_vendor,
                   lag(rule)           OVER w AS prev_rule
            FROM hospital_vendor_history
            WHERE primary_vendor IS NOT NULL
            WINDOW w AS (PARTITION BY ccn ORDER BY year)
        )
        SELECT ccn, prev_vendor AS from_vendor, prev_year AS from_year,
               primary_vendor AS to_vendor, year AS to_year,
               year - prev_year > 1 AS across_gap,
               prev_rule AS from_rule, rule AS to_rule
        FROM h
        WHERE prev_vendor IS NOT NULL AND prev_vendor <> primary_vendor"#,
];

/// Kept for callers that still reference it; enrichment tables now live in [`ENRICH_DDL`].
pub const FUTURE_DDL: &[&str] = &[];

/// Core final-table names in dependency order, used by `build` to drop-and-recreate.
pub const REBUILT_TABLES: &[&str] = &["hospital_vendor_year", "hospital_ehr", "hospitals"];

/// Enrichment final tables, each with the staging table that feeds it. `build` clears
/// and re-derives a final table only when its staging table has rows.
pub const ENRICH_TABLES: &[(&str, &str)] = &[
    ("hospital_npi", "stg_cms_enrollments"),
    ("hospital_enrollment", "stg_cms_enrollments"),
    ("hospital_geo", "stg_census_geocode"),
    ("hospital_cost_reports", "stg_hcris"),
    ("hospital_pos", "stg_cms_pos"),
    ("hospital_measures", "stg_cms_measures"),
    ("hospital_medicare_inpatient", "stg_cms_inpatient"),
    ("hospital_service_area", "stg_cms_service_area"),
    ("hospital_systems", "stg_ahrq_linkage"),
    ("health_systems", "stg_ahrq_systems"),
    ("clinician_affiliations", "stg_cms_facility_affiliation"),
    ("clinician_cehrt", "stg_onc_clinician_cehrt"),
    ("hospital_clinician_vendor", "stg_onc_clinician_cehrt"),
    ("practice_vendor", "stg_onc_clinician_cehrt"),
    ("practices", "stg_cms_dac"),
    ("practice_locations", "stg_cms_dac"),
    ("fhir_endpoints", "stg_lantern_endpoints"),
    ("practices", "stg_lantern_endpoints"),
    ("practice_locations", "stg_lantern_endpoints"),
    ("chpl_products", "stg_chpl_products"),
    ("hospital_ehr_mu", "stg_mu_attestation"),
    ("hospital_vendor_year_mu", "stg_mu_attestation"),
];
