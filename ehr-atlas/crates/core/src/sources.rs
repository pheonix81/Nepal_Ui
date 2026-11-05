//! Catalog of upstream data sources.
//!
//! URLs verified 2026-09-26. Where a catalog exposes a stable metadata API we resolve
//! the current download URL at runtime (CMS Provider Data Catalog, data.cms.gov
//! `data.json`, GitHub contents API); where it does not we hard-code the file URL (ONC).
//! Either way, the URL actually fetched is written to the manifest.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceId {
    /// CMS Provider Data Catalog "Hospital General Information" (dataset xubh-q36u).
    CmsHospitals,
    /// ONC "Certified Health IT Reported by Hospitals for Promoting Interoperability Performance"
    /// (hospital → CEHRT ID → CHPL product linkage, 2023–2024).
    OncLinkage,
    /// CMS Provider Data Catalog "Promoting Interoperability - Hospital" (dataset f4ga-b9gx).
    CmsPi,
    /// ONC CHPL REST API, `certification_ids/{cehrtId}` — resolves CEHRT IDs not covered by ONC.
    Chpl,

    // ---- enrichment, tier 1 (keyed on CCN) ----
    /// data.cms.gov "Hospital Enrollments" (monthly): CCN → NPI, legal/DBA names, practice address.
    CmsEnrollments,
    /// Census Bureau ZCTA gazetteer (ZIP centroids) — geocoding fallback.
    CensusZcta,
    /// Census Bureau batch geocoder responses (one cached CSV per batch).
    CensusGeocode,
    /// data.cms.gov "Hospital Provider Cost Report" (HCRIS), one file per fiscal year.
    Hcris,
    /// data.cms.gov "Provider of Services File – QIES" (quarterly), hospitals and other providers.
    CmsPos,
    /// CMS Provider Data Catalog quality/outcome measure datasets (several, see `CMS_MEASURE_DATASETS`).
    CmsMeasures,
    /// data.cms.gov "Medicare Inpatient Hospitals – by Provider", one file per calendar year.
    CmsInpatient,
    /// data.cms.gov "Hospital Service Area", one file per calendar year.
    CmsServiceArea,

    // ---- enrichment, tier 2 (needs a crosswalk or fuzzy match) ----
    /// AHRQ Compendium of U.S. Health Systems — hospital linkage + systems files (manual download).
    AhrqCompendium,
    /// CMS Provider Data Catalog "Facility Affiliation Data" (clinician NPI → facility CCN).
    CmsFacilityAffiliation,
    /// ONC "Certified Health IT Reported by Clinicians for PI Performance" public-use file (QPP × CHPL).
    OncClinicianCehrt,
    /// CMS Provider Data Catalog "Doctors and Clinicians National Downloadable File" (clinician × group × address).
    CmsDac,
    /// ONC Lantern daily FHIR endpoint export (archived in onc-healthit/onc-open-data).
    Lantern,
    /// ONC CHPL REST API, `search/v3` — the full certified-product catalog.
    ChplProducts,

    // ---- history ----
    /// ONC "EHR Products Used for Meaningful Use Attestation" (Medicare EHR Incentive Program × CHPL, 2011–2016).
    MuAttestation,
}

impl SourceId {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceId::CmsHospitals => "cms_hospitals",
            SourceId::OncLinkage => "onc_linkage",
            SourceId::CmsPi => "cms_pi",
            SourceId::Chpl => "chpl",
            SourceId::CmsEnrollments => "cms_enrollments",
            SourceId::CensusZcta => "census_zcta",
            SourceId::CensusGeocode => "census_geocode",
            SourceId::Hcris => "hcris",
            SourceId::CmsPos => "cms_pos",
            SourceId::CmsMeasures => "cms_measures",
            SourceId::CmsInpatient => "cms_inpatient",
            SourceId::CmsServiceArea => "cms_service_area",
            SourceId::AhrqCompendium => "ahrq_compendium",
            SourceId::CmsFacilityAffiliation => "cms_facility_affiliation",
            SourceId::OncClinicianCehrt => "onc_clinician_cehrt",
            SourceId::CmsDac => "cms_dac",
            SourceId::Lantern => "lantern",
            SourceId::ChplProducts => "chpl_products",
            SourceId::MuAttestation => "mu_attestation",
        }
    }
}

// ---------------------------------------------------------------------------
// CMS Provider Data Catalog (data.cms.gov/provider-data) — Care Compare datasets
// ---------------------------------------------------------------------------

/// CMS Provider Data Catalog metastore. `{id}` is the dataset identifier.
pub const CMS_PDC_METASTORE: &str =
    "https://data.cms.gov/provider-data/api/1/metastore/schemas/dataset/items/{id}?show-reference-ids=false";
pub const CMS_HOSPITALS_DATASET_ID: &str = "xubh-q36u";
pub const CMS_PI_HOSPITAL_DATASET_ID: &str = "f4ga-b9gx";
/// "Facility Affiliation Data": clinician NPI → facility CCN (`facility_affiliations_certification_number`).
pub const CMS_FACILITY_AFFILIATION_DATASET_ID: &str = "27ea-46a8";
/// "Doctors and Clinicians National Downloadable File": one row per clinician × enrollment ×
/// group (`org_pac_id`, legal name, member count) × practice address (~2.7M rows, ~840 MB).
/// Data dictionary: https://data.cms.gov/provider-data/sites/default/files/data_dictionaries/physician/DOC_Data_Dictionary.pdf
pub const CMS_DAC_DATASET_ID: &str = "mj5m-pzi6";

/// Care Compare measure datasets loaded by `cms-measures` into the long `hospital_measures`
/// table. `(dataset_id, short name)`. Column mappings live in `config/measures.csv`.
pub const CMS_MEASURE_DATASETS: &[(&str, &str)] = &[
    ("dgck-syfz", "hcahps"),
    ("9n3s-kdb3", "hrrp"),
    ("ynj2-r877", "complications_deaths"),
    ("yq43-i98g", "hac"),
    ("ypbt-wvdk", "hvbp_tps"),
    ("632h-zaca", "unplanned_visits"),
    ("yv7e-xc69", "timely_effective"),
    ("nrth-mfg3", "spending_by_claim"),
    ("rrqw-56er", "mspb"),
];

// ---------------------------------------------------------------------------
// data.cms.gov (the *other* CMS catalog) — resolved through its DCAT `data.json`
// ---------------------------------------------------------------------------

/// DCAT catalog listing every data.cms.gov dataset with per-release CSV distributions.
pub const DATA_CMS_GOV_CATALOG: &str = "https://data.cms.gov/data.json";
/// Dataset UUIDs on data.cms.gov (the `identifier` field is
/// `https://data.cms.gov/data-api/v1/dataset/<uuid>/data-viewer`).
pub const CMS_ENROLLMENTS_DATASET_UUID: &str = "f6f6505c-e8b0-4d57-b258-e2b94133aaf2";
pub const HCRIS_DATASET_UUID: &str = "44060663-47d8-4ced-a115-b53b4c270acb";
pub const CMS_POS_DATASET_UUID: &str = "8ba0f9b4-9493-4aa0-9f82-44ea9468d1b5";
pub const CMS_INPATIENT_DATASET_UUID: &str = "ee6fb1a5-39b9-46b3-a980-a7284551a732";
pub const CMS_SERVICE_AREA_DATASET_UUID: &str = "8708ca8b-8636-44ed-8303-724cbfaf78ad";

// ---------------------------------------------------------------------------
// ONC
// ---------------------------------------------------------------------------

/// ONC hospital→CHPL linkage CSV. The dataset page is
/// https://healthit.gov/data/datasets/certified-health-information-technology-reported-by-hospitals-for-promoting-interoperability-performance/
pub const ONC_LINKAGE_CSV: &str =
    "https://healthit.gov/wp-content/uploads/data/datasets/hospital-promoting-interoperability-chpl-linkage.csv";

/// CHPL REST base. Requires an `API-Key` header; register at
/// https://chpl.healthit.gov/#/resources/api (free, key is emailed).
pub const CHPL_API_BASE: &str = "https://chpl.healthit.gov/rest";

/// ONC open-data repo holding the clinician CEHRT public-use files and Lantern exports.
/// Files are stored in Git LFS, so download through the media host, not `raw.githubusercontent.com`.
pub const ONC_OPEN_DATA_REPO: &str = "onc-healthit/onc-open-data";
pub const ONC_OPEN_DATA_MEDIA: &str =
    "https://media.githubusercontent.com/media/onc-healthit/onc-open-data/main/";
pub const GITHUB_CONTENTS_API: &str = "https://api.github.com/repos/{repo}/contents/{path}";
/// ONC "EHR Products Used for Meaningful Use Attestation": one row per provider × program
/// year × certified product, hospitals (with CCN) and eligible professionals, program years
/// 2011–2016 (a handful of 2017 stragglers). Not UTF-8 (Windows-1252 ® / ™ bytes). Dataset page:
/// https://healthit.gov/data/datasets/ehr-products-used-meaningful-use-attestation/
/// (the older `/sites/default/files/data/datasets/MU_REPORT.csv` URL now returns 404).
pub const ONC_MU_ATTESTATION_CSV: &str =
    "https://healthit.gov/wp-content/uploads/data/datasets/MU_REPORT.csv";

/// Clinician CEHRT PUF: `puf/<year>/clinician-certified-technology-pi-<year>-<part>.csv`, 4 parts/year.
pub const ONC_CLINICIAN_PUF_DIR: &str = "puf";
/// Lantern daily export: `lantern-daily-data/<YYYY>/<Month>/<MM_DD_YYYY>endpointdata.csv`.
pub const LANTERN_DAILY_DIR: &str = "lantern-daily-data";

// ---------------------------------------------------------------------------
// Census
// ---------------------------------------------------------------------------

/// ZCTA gazetteer (tab-separated; `GEOID`, `INTPTLAT`, `INTPTLONG`).
pub const CENSUS_ZCTA_GAZETTEER_ZIP: &str =
    "https://www2.census.gov/geo/docs/maps-data/data/gazetteer/2024_Gazetteer/2024_Gaz_zcta_national.zip";
/// Batch geocoder: multipart POST with `addressFile` (CSV: id,street,city,state,zip; ≤10,000 rows)
/// and `benchmark`. Returns CSV: id, input, match, match_type, matched_address, "lon,lat", tiger_id, side.
pub const CENSUS_GEOCODER_BATCH: &str =
    "https://geocoding.geo.census.gov/geocoder/locations/addressbatch";
pub const CENSUS_GEOCODER_BENCHMARK: &str = "Public_AR_Current";

// ---------------------------------------------------------------------------
// AHRQ
// ---------------------------------------------------------------------------

/// AHRQ Compendium page (per year: `compendium-<year>.html`). ahrq.gov answers clients
/// that do not look like a browser with a bot challenge (HTTP 202/403); the CSV files
/// themselves download fine when the request carries [`BROWSER_USER_AGENT`]. `--file`
/// remains as a fallback for a hand-downloaded copy.
pub const AHRQ_COMPENDIUM_PAGE: &str = "https://www.ahrq.gov/chsp/data-resources/compendium-{year}.html";
/// Hospital Linkage File: one row per hospital (CCN, name, address, health_sys_id, beds, …).
pub const AHRQ_LINKAGE_CSV: &str =
    "https://www.ahrq.gov/sites/default/files/wysiwyg/chsp/compendium/chsp-hospital-linkage-{year}.csv";
/// Health Systems file: one row per system (id, name, counts, flags).
pub const AHRQ_SYSTEMS_CSV: &str =
    "https://www.ahrq.gov/sites/default/files/wysiwyg/chsp/compendium/chsp-compendium-{year}.csv";
/// Newest compendium year known to exist (verified 2026-09-26).
pub const AHRQ_LATEST_YEAR: u16 = 2023;
/// User-Agent presented to hosts that reject non-browser clients (ahrq.gov).
pub const BROWSER_USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0 Safari/537.36";

// ---------------------------------------------------------------------------
// Expected header rows. The pipeline refuses to load a file whose header does not
// contain these columns, so upstream schema drift fails loudly instead of silently
// producing empty joins.
// ---------------------------------------------------------------------------

pub const CMS_HOSPITALS_REQUIRED_COLUMNS: &[&str] = &[
    "Facility ID",
    "Facility Name",
    "Address",
    "City/Town",
    "State",
    "ZIP Code",
    "County/Parish",
    "Hospital Type",
    "Hospital Ownership",
    "Emergency Services",
];

pub const ONC_LINKAGE_REQUIRED_COLUMNS: &[&str] = &[
    "Facility.ID",
    "Facility.Name",
    "State",
    "Meets.criteria.for.promoting.interoperability.of.EHRs",
    "Start.Date",
    "End.Date",
    "CEHRT.ID",
    "chpl_id",
    "product_database_id",
    "developer_name",
    "product_name",
    "year",
];

pub const CMS_PI_REQUIRED_COLUMNS: &[&str] = &[
    "Facility ID",
    "Facility Name",
    "State",
    "CEHRT ID",
    "Meets criteria for promoting interoperability of EHRs",
    "Start Date",
    "End Date",
];

pub const CMS_ENROLLMENTS_REQUIRED_COLUMNS: &[&str] = &[
    "ENROLLMENT ID",
    "NPI",
    "CCN",
    "ORGANIZATION NAME",
    "DOING BUSINESS AS NAME",
    "INCORPORATION DATE",
    "PROPRIETARY NONPROFIT",
    "ADDRESS LINE 1",
    "CITY",
    "STATE",
    "ZIP CODE",
    "CAH OR HOSPITAL CCN",
];

pub const HCRIS_REQUIRED_COLUMNS: &[&str] = &[
    "Provider CCN",
    "Fiscal Year Begin Date",
    "Fiscal Year End Date",
    "Number of Beds",
    "Total Discharges (V + XVIII + XIX + Unknown)",
    "Net Patient Revenue",
    "Total Costs",
];

pub const CMS_POS_REQUIRED_COLUMNS: &[&str] = &[
    "PRVDR_NUM",
    "PRVDR_CTGRY_CD",
    "PRVDR_CTGRY_SBTYP_CD",
    "PGM_TRMNTN_CD",
    "CBSA_URBN_RRL_IND",
    "BED_CNT",
    "CRTFD_BED_CNT",
];

pub const CMS_INPATIENT_REQUIRED_COLUMNS: &[&str] = &[
    "Rndrng_Prvdr_CCN",
    "Rndrng_Prvdr_RUCA",
    "Tot_Benes",
    "Tot_Dschrgs",
    "Tot_Mdcr_Pymt_Amt",
];

pub const AHRQ_LINKAGE_REQUIRED_COLUMNS: &[&str] = &["ccn", "hospital_name", "health_sys_id", "health_sys_name"];
pub const AHRQ_SYSTEMS_REQUIRED_COLUMNS: &[&str] = &["health_sys_id", "health_sys_name", "health_sys_state"];

pub const CMS_FACILITY_AFFILIATION_REQUIRED_COLUMNS: &[&str] = &[
    "NPI",
    "facility_type",
    "Facility Affiliations Certification Number",
];

pub const CMS_DAC_REQUIRED_COLUMNS: &[&str] = &[
    "NPI",
    "Ind_PAC_ID",
    "Provider Last Name",
    "Provider First Name",
    "pri_spec",
    "Facility Name",
    "org_pac_id",
    "num_org_mem",
    "adr_ln_1",
    "City/Town",
    "State",
    "ZIP Code",
    "adrs_id",
];

pub const ONC_CLINICIAN_PUF_REQUIRED_COLUMNS: &[&str] = &[
    "npi",
    "chpl_id",
    "developer",
    "product",
    "edition",
];

pub const ONC_MU_ATTESTATION_REQUIRED_COLUMNS: &[&str] = &[
    "NPI",
    "CCN",
    "Provider_Type",
    "Program_Year",
    "EHR_Certification_Number",
    "EHR_Product_CHP_Id",
    "Vendor_Name",
    "EHR_Product_Name",
    "Product_Classification",
    "Product_Setting",
];
