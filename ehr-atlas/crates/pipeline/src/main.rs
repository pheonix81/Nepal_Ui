//! `ehr-atlas` — pipeline CLI.
//!
//! One subcommand per source (download → normalize into a staging table), `build` to
//! produce the final tables, `report` for the data-quality report, `run-all` for the
//! core pipeline and `enrich-all` for every enrichment source.

mod build;
mod config;
mod db;
mod download;
mod export_ui;
mod report;
mod sources;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use config::Paths;
use ehr_atlas_core::manifest::{Manifest, RawFile};
use std::path::PathBuf;
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(
    name = "ehr-atlas",
    version,
    about = "U.S. hospital EHR market data pipeline"
)]
struct Cli {
    /// Repository root (defaults to the nearest ancestor containing Cargo.toml + crates/).
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    /// Do not hit the network; reuse the latest downloaded file for each source.
    #[arg(long, global = true)]
    offline: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Debug, Clone)]
struct ChplArgs {
    /// CHPL API key (free: https://chpl.healthit.gov/#/resources/api).
    #[arg(long, env = "CHPL_API_KEY", hide_env_values = true)]
    api_key: Option<String>,
    /// Resolve every CEHRT ID seen, not just the ones ONC left unlinked.
    #[arg(long)]
    all: bool,
    /// Minimum milliseconds between CHPL requests.
    #[arg(long, default_value_t = 1000)]
    min_interval_ms: u64,
    /// Fail (instead of warn) if any CEHRT ID cannot be resolved.
    #[arg(long)]
    strict: bool,
}

#[derive(Args, Debug, Clone)]
struct AhrqArgs {
    /// Hospital Linkage File CSV downloaded by hand from the AHRQ Compendium page.
    #[arg(long)]
    file: Option<PathBuf>,
    /// Health Systems file CSV (optional; adds system-level attributes).
    #[arg(long)]
    systems_file: Option<PathBuf>,
    /// Compendium year to download (or the year the hand-supplied files belong to).
    #[arg(long, default_value_t = ehr_atlas_core::sources::AHRQ_LATEST_YEAR)]
    year: u16,
}

#[derive(Args, Debug, Clone)]
struct ChplProductsArgs {
    /// CHPL API key (free: https://chpl.healthit.gov/#/resources/api).
    #[arg(long, env = "CHPL_API_KEY", hide_env_values = true)]
    api_key: Option<String>,
    /// Re-fetch every page even if cached.
    #[arg(long)]
    refresh: bool,
    /// Minimum milliseconds between CHPL requests.
    #[arg(long, default_value_t = 1000)]
    min_interval_ms: u64,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    // ---- core ----
    /// CMS Hospital General Information → stg_cms_hospitals
    CmsHospitals,
    /// ONC hospital→CHPL linkage (2023–2024) → stg_onc_linkage
    OncLinkage,
    /// CMS Promoting Interoperability – Hospital → stg_cms_pi
    CmsPi,
    /// Resolve unlinked CEHRT IDs via the CHPL API (cached) → stg_chpl_cehrt
    Chpl(ChplArgs),

    // ---- enrichment, tier 1 (keyed on CCN) ----
    /// data.cms.gov Hospital Enrollments → stg_cms_enrollments (CCN→NPI, addresses)
    CmsEnrollments,
    /// Census batch geocoder + ZCTA centroids → stg_census_geocode, stg_census_zcta
    Geocode,
    /// HCRIS Hospital Provider Cost Report → stg_hcris (beds, discharges, revenue)
    Hcris,
    /// Provider of Services (QIES) → stg_cms_pos (certified beds, accreditation, urban/rural)
    CmsPos,
    /// Care Compare measure datasets → stg_cms_measures (long format)
    CmsMeasures,
    /// Medicare Inpatient Hospitals by Provider → stg_cms_inpatient
    CmsInpatient,
    /// Hospital Service Area → stg_cms_service_area (patient origin by ZIP)
    CmsServiceArea,

    // ---- enrichment, tier 2 ----
    /// AHRQ Compendium hospital linkage + systems files → stg_ahrq_linkage, stg_ahrq_systems
    AhrqCompendium(AhrqArgs),
    /// Facility Affiliation + ONC clinician CEHRT PUF → stg_cms_facility_affiliation, stg_onc_clinician_cehrt
    Clinicians,
    /// CMS Doctors & Clinicians National Downloadable File → stg_cms_dac (practice names, addresses)
    CmsDac,
    /// ONC Lantern daily FHIR endpoint export → stg_lantern_endpoints, stg_lantern_orgs
    Lantern,
    /// CHPL certified-product catalog via search API (cached) → stg_chpl_products
    ChplProducts(ChplProductsArgs),

    // ---- history ----
    /// ONC Meaningful Use attestations (2011–2016, hospitals) → stg_mu_attestation
    MuAttestation,

    // ---- assembly ----
    /// Build hospitals / hospital_ehr / hospital_vendor_year, derive every enrichment
    /// table whose staging is loaded, recreate views, export Parquet/CSV
    Build,
    /// Write reports/data_quality.md (and reports/market.md when enrichment is loaded)
    Report,
    /// Write data/processed/ui.json and render web/index.html → web/dist/index.html
    ExportUi,
    /// Core pipeline: the four core sources, then build + report
    RunAll(ChplArgs),
    /// Every enrichment source that needs no manual input, then build + report + export-ui
    EnrichAll(ChplProductsArgs),
    /// Print the raw-file manifest
    Manifest,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();
    let cli = Cli::parse();
    let paths = Paths::new(cli.root.clone())?;
    paths.ensure_dirs()?;
    info!(root = %paths.root.display(), db = %paths.db.display(), "ehr-atlas");
    let http = download::http_client()?;
    let offline = cli.offline;

    match cli.cmd {
        Cmd::CmsHospitals => cms_hospitals(&paths, &http, offline).await?,
        Cmd::OncLinkage => onc_linkage(&paths, &http, offline).await?,
        Cmd::CmsPi => cms_pi(&paths, &http, offline).await?,
        Cmd::Chpl(a) => chpl(&paths, &http, offline, &a).await?,

        Cmd::CmsEnrollments => run_source!(cms_enrollments, &paths, &http, offline),
        Cmd::Geocode => run_source!(geocode, &paths, &http, offline),
        Cmd::Hcris => run_source!(hcris, &paths, &http, offline),
        Cmd::CmsPos => run_source!(cms_pos, &paths, &http, offline),
        Cmd::CmsMeasures => run_source!(cms_measures, &paths, &http, offline),
        Cmd::CmsInpatient => run_source!(cms_inpatient, &paths, &http, offline),
        Cmd::CmsServiceArea => run_source!(cms_service_area, &paths, &http, offline),

        Cmd::AhrqCompendium(a) => ahrq(&paths, &http, offline, &a).await?,
        Cmd::Clinicians => run_source!(clinicians, &paths, &http, offline),
        Cmd::CmsDac => run_source!(cms_dac, &paths, &http, offline),
        Cmd::Lantern => run_source!(lantern, &paths, &http, offline),
        Cmd::ChplProducts(a) => chpl_products(&paths, &http, offline, &a).await?,
        Cmd::MuAttestation => run_source!(mu_attestation, &paths, &http, offline),

        Cmd::Build => build::build(&db::open(&paths)?, &paths)?,
        Cmd::Report => {
            let out = report::write_report(&db::open(&paths)?, &paths)?;
            info!(path = %out.display(), "report written");
        }
        Cmd::ExportUi => {
            let out = export_ui::export(&db::open(&paths)?, &paths)?;
            info!(path = %out.display(), "ui exported");
        }
        Cmd::RunAll(a) => {
            cms_hospitals(&paths, &http, offline).await?;
            onc_linkage(&paths, &http, offline).await?;
            cms_pi(&paths, &http, offline).await?;
            chpl(&paths, &http, offline, &a).await?;
            let conn = db::open(&paths)?;
            build::build(&conn, &paths)?;
            let out = report::write_report(&conn, &paths)?;
            info!(path = %out.display(), "report written");
        }
        Cmd::EnrichAll(a) => {
            run_source!(cms_enrollments, &paths, &http, offline);
            run_source!(hcris, &paths, &http, offline);
            run_source!(cms_pos, &paths, &http, offline);
            run_source!(cms_measures, &paths, &http, offline);
            run_source!(cms_inpatient, &paths, &http, offline);
            run_source!(cms_service_area, &paths, &http, offline);
            run_source!(clinicians, &paths, &http, offline);
            run_source!(cms_dac, &paths, &http, offline);
            // geocode after clinicians + cms-dac so practice locations are in the batch
            run_source!(geocode, &paths, &http, offline);
            run_source!(lantern, &paths, &http, offline);
            chpl_products(&paths, &http, offline, &a).await?;
            run_source!(mu_attestation, &paths, &http, offline);
            let conn = db::open(&paths)?;
            build::build(&conn, &paths)?;
            let out = report::write_report(&conn, &paths)?;
            info!(path = %out.display(), "report written");
            let page = export_ui::export(&conn, &paths)?;
            info!(path = %page.display(), "ui exported");
        }
        Cmd::Manifest => {
            let m = Manifest::load(&paths.manifest)?;
            println!("{}", serde_json::to_string_pretty(&m)?);
        }
    }
    Ok(())
}

/// fetch → sync manifest → normalize, for any module following the enrichment contract.
macro_rules! run_source {
    ($m:ident, $paths:expr, $http:expr, $offline:expr) => {{
        let files: Vec<RawFile> = sources::$m::fetch($paths, $http, $offline).await?;
        let conn = db::open($paths)?;
        db::sync_manifest(&conn, $paths)?;
        sources::$m::normalize(&conn, $paths, &files)?;
    }};
}
use run_source;

async fn cms_hospitals(paths: &Paths, http: &reqwest::Client, offline: bool) -> Result<()> {
    let file = sources::cms_hospitals::fetch(paths, http, offline).await?;
    let conn = db::open(paths)?;
    db::sync_manifest(&conn, paths)?;
    sources::cms_hospitals::normalize(&conn, paths, &file)
}

async fn onc_linkage(paths: &Paths, http: &reqwest::Client, offline: bool) -> Result<()> {
    let file = sources::onc_linkage::fetch(paths, http, offline).await?;
    let conn = db::open(paths)?;
    db::sync_manifest(&conn, paths)?;
    sources::onc_linkage::normalize(&conn, paths, &file)
}

async fn cms_pi(paths: &Paths, http: &reqwest::Client, offline: bool) -> Result<()> {
    sources::cms_pi::fetch(paths, http, offline).await?;
    let conn = db::open(paths)?;
    db::sync_manifest(&conn, paths)?;
    let manifest = Manifest::load(&paths.manifest)?;
    let files: Vec<_> = manifest
        .files
        .iter()
        .filter(|f| f.source == sources::cms_pi::SOURCE && paths.root.join(&f.path).exists())
        .cloned()
        .collect();
    sources::cms_pi::normalize_all(&conn, paths, &files)
}

async fn chpl(paths: &Paths, http: &reqwest::Client, offline: bool, args: &ChplArgs) -> Result<()> {
    let conn = db::open(paths)?;
    let mut client = sources::chpl::ChplClient::new(
        paths,
        http.clone(),
        args.api_key.clone(),
        args.min_interval_ms,
    )?;
    let ids = if args.all {
        sources::chpl::all_ids(&conn)?
    } else {
        sources::chpl::unresolved_ids(&conn)?
    };
    let mut missing = Vec::new();
    for id in &ids {
        if client.read_cache(id)?.is_some() {
            continue;
        }
        if offline || !client.has_key() {
            missing.push(id.clone());
            continue;
        }
        client.resolve(id).await?;
    }
    let (n_ids, n_rows) = sources::chpl::load_cache_into_staging(&conn, &client)?;
    info!(
        needed = ids.len(),
        cached_ids = n_ids,
        staged_rows = n_rows,
        "stg_chpl_cehrt loaded from cache"
    );
    if !missing.is_empty() {
        let msg = format!(
            "{} CEHRT ID(s) need the CHPL API but {}:\n  {}\n  Get a free key at https://chpl.healthit.gov/#/resources/api \
             (register with name + email; the key is emailed), then re-run:\n  CHPL_API_KEY=... ehr-atlas chpl",
            missing.len(),
            if offline { "--offline was set" } else { "CHPL_API_KEY is not set" },
            missing.join(" ")
        );
        if args.strict {
            anyhow::bail!("{msg}");
        }
        warn!("{msg}");
    }
    Ok(())
}

async fn ahrq(paths: &Paths, http: &reqwest::Client, offline: bool, args: &AhrqArgs) -> Result<()> {
    let files = sources::ahrq_compendium::fetch(
        paths,
        http,
        offline,
        args.file.as_deref(),
        args.systems_file.as_deref(),
        args.year,
    )
    .await?;
    let conn = db::open(paths)?;
    db::sync_manifest(&conn, paths)?;
    sources::ahrq_compendium::normalize(&conn, paths, &files)
}

async fn chpl_products(
    paths: &Paths,
    http: &reqwest::Client,
    offline: bool,
    args: &ChplProductsArgs,
) -> Result<()> {
    let conn = db::open(paths)?;
    sources::chpl_products::sync(
        &conn,
        paths,
        http,
        args.api_key.as_deref(),
        offline,
        args.refresh,
        args.min_interval_ms,
    )
    .await
}
