//! ╔══════════════════════════════════════════════════════════════════╗
//! ║          Daily News Media Station — Axum Web Server             ║
//! ║                                                                  ║
//! ║  Routes:                                                         ║
//! ║    GET  /            → serves ./frontend/ (static SPA)           ║
//! ║    GET  /media/*     → serves ./data/     (EPUB + MP3 files)     ║
//! ║    GET  /api/media   → JSON list of available dated media files  ║
//! ║    GET  /opds        → OPDS catalog of EPUBs for e-readers       ║
//! ║                                                                  ║
//! ║  Background task: every 6 h, delete data files older than 10 d  ║
//! ╚══════════════════════════════════════════════════════════════════╝

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use std::sync::atomic::{AtomicBool, Ordering};
use chrono::{DateTime, Duration, Timelike, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::time;
use tower_http::{cors::CorsLayer, services::ServeDir, trace::TraceLayer};
use tracing::{error, info, warn};

// ═══════════════════════════════════════════════════════════════════
// SHARED APPLICATION STATE
// ═══════════════════════════════════════════════════════════════════

/// Shared references to directory paths, cheaply cloneable via Arc.
#[derive(Clone)]
struct AppState {
    data_dir: Arc<PathBuf>,
    is_scraping: Arc<AtomicBool>,
    scraper_logs: Arc<tokio::sync::Mutex<std::collections::VecDeque<String>>>,
    last_run_success: Arc<AtomicBool>,
}

// ═══════════════════════════════════════════════════════════════════
// API TYPES
// ═══════════════════════════════════════════════════════════════════

/// A set of media files associated with a single calendar date (YYYYMMDD).
#[derive(Debug, Serialize, Deserialize)]
struct MediaEntry {
    /// Date string in YYYYMMDD format, e.g. "20260628"
    date:    String,
    /// Filename of the EPUB book, if generated for this date
    epub:    Option<String>,
    /// Filename of the companion TLDR digest EPUB, if generated
    tldr:    Option<String>,
    /// Filename of the short radio briefing MP3, if generated
    radio:   Option<String>,
    /// Filename of the long podcast MP3, if generated
    podcast: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RssFeed {
    name: String,
    url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppConfig {
    rss_feeds: Vec<RssFeed>,
    medium_tags: Vec<String>,
    #[serde(default)]
    silenced_sources: Vec<String>,
    /// Presentation order for sources in the EPUB chapters and audio scripts;
    /// unlisted sources follow in scrape order. Consumed by the scraper.
    #[serde(default)]
    source_order: Vec<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    // Voice + per-briefing source selection (previously browser localStorage;
    // stored here so settings are global rather than per-browser)
    #[serde(default)]
    voice_short: Option<String>,
    #[serde(default)]
    voice_long: Option<String>,
    #[serde(default)]
    sources_short: Vec<String>,
    #[serde(default)]
    sources_long: Vec<String>,
    #[serde(default = "default_skip_paywalled")]
    skip_paywalled_posts: bool,
    /// Serve the /opds catalog (Settings toggle). Default on — it's the
    /// pull path for e-readers; disable if you don't want it exposed.
    #[serde(default = "default_opds_enabled")]
    opds_enabled: bool,
    // Per-task LLM backend overrides (None = server's LLM_BACKEND env) and
    // per-output enable flags. Consumed by the scraper via config.json.
    #[serde(default)]
    llm_radio: Option<String>,
    #[serde(default)]
    llm_podcast: Option<String>,
    #[serde(default)]
    llm_tldr: Option<String>,
    #[serde(default = "default_true")]
    enable_radio: bool,
    #[serde(default = "default_true")]
    enable_podcast: bool,
    #[serde(default = "default_true")]
    enable_tldr: bool,
    /// Days without activity before a source is flagged as dead in the health panel.
    #[serde(default = "default_source_health_dead_days")]
    source_health_dead_days: u32,
    /// Hour (UTC, 0–23) at which the internal daily scheduler fires the scraper.
    #[serde(default = "default_daily_run_hour")]
    daily_run_hour: u8,
    /// Minute (0–59) at which the internal daily scheduler fires the scraper.
    #[serde(default)]
    daily_run_minute: u8,
    /// Days generated media is kept before the cleanup task deletes it.
    #[serde(default = "default_cleanup_max_age_days")]
    cleanup_max_age_days: u32,

    // ── ntfy push notifications ──────────────────────────────────
    /// Master switch. Off = no outbound requests are made at all.
    #[serde(default)]
    ntfy_enabled: bool,
    /// Base URL of the ntfy server — ntfy.sh or a self-hosted instance.
    #[serde(default = "default_ntfy_server")]
    ntfy_server: String,
    /// Topic to publish to. Anyone who knows a public topic name can read it,
    /// so treat it as a secret on ntfy.sh unless the topic is access-controlled.
    #[serde(default)]
    ntfy_topic: String,
    /// Optional bearer token (`tk_…`) for a protected topic.
    #[serde(default)]
    ntfy_token: Option<String>,
    /// Notify when the internal daily scheduler fires a run.
    #[serde(default = "default_true")]
    ntfy_on_scheduled_run: bool,
    /// Notify when a run is started by hand from the dashboard.
    #[serde(default)]
    ntfy_on_manual_run: bool,
    /// Notify when a run finishes successfully.
    #[serde(default = "default_true")]
    ntfy_on_success: bool,
    /// Notify when a run fails, including the captured error lines.
    #[serde(default = "default_true")]
    ntfy_on_failure: bool,
    /// Notify when a failure looks like expired/invalid LLM credentials.
    #[serde(default = "default_true")]
    ntfy_on_token_expiry: bool,

    /// Which ntfy connection fields the environment is currently overriding.
    ///
    /// Response-only: the UI uses it to lock those inputs, because editing them
    /// would otherwise appear to save and then silently revert on reload — the
    /// environment wins in `apply_ntfy_env_overrides`. Never read from
    /// config.json, and never written back to it.
    #[serde(default, skip_deserializing)]
    ntfy_env_locked: Vec<String>,
}

fn default_true() -> bool {
    true
}

fn default_skip_paywalled() -> bool {
    true
}

fn default_opds_enabled() -> bool {
    true
}

fn default_source_health_dead_days() -> u32 {
    30
}

fn default_daily_run_hour() -> u8 {
    6
}

fn default_cleanup_max_age_days() -> u32 {
    10
}

fn default_ntfy_server() -> String {
    "https://ntfy.sh".to_string()
}

/// Read config.json from disk, returning defaults on any error.
fn load_app_config(data_dir: &std::path::Path) -> AppConfig {
    let path = data_dir.join("config.json");
    let mut cfg = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<AppConfig>(&s).ok())
        .unwrap_or_default();
    apply_ntfy_env_overrides(&mut cfg);
    cfg
}

/// Let the environment override the ntfy connection settings from config.json.
///
/// The topic and token are credentials, and config.json lives on a PVC — so it
/// cannot be managed from git and is lost with the volume. Reading them from the
/// environment instead lets them come from a secret store (Vault via
/// ExternalSecret) while the UI keeps owning every other setting.
///
/// Supplying NTFY_TOPIC also switches notifications on: an operator who injected
/// a topic clearly wants them, and `ntfy_enabled` defaults to false, which would
/// otherwise silently discard every message. Set NTFY_ENABLED=false to override
/// that.
fn apply_ntfy_env_overrides(cfg: &mut AppConfig) {
    fn env_non_empty(key: &str) -> Option<String> {
        std::env::var(key).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
    }

    if let Some(server) = env_non_empty("NTFY_SERVER") {
        cfg.ntfy_server = server;
    }
    if let Some(topic) = env_non_empty("NTFY_TOPIC") {
        cfg.ntfy_topic = topic;
        cfg.ntfy_enabled = true;
    }
    if let Some(token) = env_non_empty("NTFY_TOKEN") {
        cfg.ntfy_token = Some(token);
    }
    // Explicit switch always wins, so the injected topic can be muted without
    // tearing the secret back out of the deployment.
    if let Some(enabled) = env_non_empty("NTFY_ENABLED") {
        cfg.ntfy_enabled = matches!(
            enabled.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        );
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            rss_feeds: vec![
                RssFeed {
                    name: "BBC News".to_string(),
                    url: "http://feeds.bbci.co.uk/news/rss.xml".to_string(),
                },
                RssFeed {
                    name: "Azure DevOps Blog".to_string(),
                    url: "https://devblogs.microsoft.com/devops/feed/".to_string(),
                },
                RssFeed {
                    name: "GitHub Engineering Blog".to_string(),
                    url: "https://github.blog/feed/".to_string(),
                },
                RssFeed {
                    name: "CNCF Blog".to_string(),
                    url: "https://www.cncf.io/feed/".to_string(),
                },
                RssFeed {
                    name: "Kubernetes Blog".to_string(),
                    url: "https://kubernetes.io/feed.xml".to_string(),
                },
                RssFeed {
                    name: "Google Cloud Tech Blog".to_string(),
                    url: "https://cloudblog.withgoogle.com/rss".to_string(),
                },
                RssFeed {
                    name: "HashiCorp Blog".to_string(),
                    url: "https://www.hashicorp.com/blog/feed.xml".to_string(),
                },
                RssFeed {
                    name: "Ansible Blog".to_string(),
                    url: "https://www.ansible.com/blog/rss.xml".to_string(),
                },
                RssFeed {
                    name: "Red Hat Blog".to_string(),
                    url: "https://www.redhat.com/en/blog/rss.xml".to_string(),
                },
                RssFeed {
                    name: "NGINX Blog".to_string(),
                    url: "https://www.nginx.com/blog/feed/".to_string(),
                },
                RssFeed {
                    name: "Canonical Ubuntu Blog".to_string(),
                    url: "https://ubuntu.com/blog/feed".to_string(),
                },
                RssFeed {
                    name: "Let's Do DevOps".to_string(),
                    url: "https://letsdodevops.substack.com/feed".to_string(),
                },
                RssFeed {
                    name: "DevOps Daily".to_string(),
                    url: "https://devopsdaily.substack.com/feed".to_string(),
                },
                RssFeed {
                    name: "DevOps Bulletin".to_string(),
                    url: "https://devopsbulletin.substack.com/feed".to_string(),
                },
                RssFeed {
                    name: "Terraform Blog".to_string(),
                    url: "https://www.hashicorp.com/blog/category/terraform/feed".to_string(),
                },
                RssFeed {
                    name: "DevOpsCube".to_string(),
                    url: "https://devopscube.com/feed/".to_string(),
                },
                RssFeed {
                    name: "Daily Mail".to_string(),
                    url: "https://www.dailymail.com/articles.rss".to_string(),
                },
            ],
            medium_tags: vec!["terraform".to_string()],
            silenced_sources: Vec::new(),
            source_order: Vec::new(),
            system_prompt: None,
            voice_short: None,
            voice_long: None,
            sources_short: Vec::new(),
            sources_long: Vec::new(),
            skip_paywalled_posts: true,
            opds_enabled: true,
            llm_radio: None,
            llm_podcast: None,
            llm_tldr: None,
            enable_radio: true,
            enable_podcast: true,
            enable_tldr: true,
            source_health_dead_days: 30,
            daily_run_hour: 6,
            daily_run_minute: 0,
            cleanup_max_age_days: 10,
            ntfy_enabled: false,
            ntfy_server: default_ntfy_server(),
            ntfy_topic: String::new(),
            ntfy_token: None,
            ntfy_on_scheduled_run: true,
            ntfy_on_manual_run: false,
            ntfy_on_success: true,
            ntfy_on_failure: true,
            ntfy_on_token_expiry: true,
            ntfy_env_locked: Vec::new(),
        }
    }
}

/// Top-level API response wrapping an ordered list of media entries.
#[derive(Serialize)]
struct MediaListResponse {
    /// Entries are sorted newest-first.
    dates: Vec<MediaEntry>,
}

// ═══════════════════════════════════════════════════════════════════
// MEDIA LISTING
// ═══════════════════════════════════════════════════════════════════

/// Scan `data_dir` and group files by their embedded YYYYMMDD date.
///
/// Recognised filename patterns:
///   - `daily-news-YYYYMMDD.epub`
///   - `short-radio-YYYYMMDD.mp3`
///   - `long-podcast-YYYYMMDD.mp3`
fn list_media_files(data_dir: &Path) -> Vec<MediaEntry> {
    // BTreeMap ensures dates are iterated in lexicographic (chronological) order.
    let mut groups: BTreeMap<String, MediaEntry> = BTreeMap::new();

    let date_re = Regex::new(r"(\d{8}-\d{6}|\d{8})").expect("Invalid date regex");

    let read_dir = match std::fs::read_dir(data_dir) {
        Ok(rd) => rd,
        Err(e) => {
            warn!("Cannot read data directory {:?}: {}", data_dir, e);
            return Vec::new();
        }
    };

    for entry in read_dir.flatten() {
        let file_name = entry.file_name().to_string_lossy().to_string();

        // Only process recognised file types
        if !file_name.ends_with(".epub") && !file_name.ends_with(".mp3") {
            continue;
        }

        // Extract the 8-digit date embedded in the filename
        let date = match date_re.find(&file_name) {
            Some(m) => m.as_str().to_string(),
            None    => continue,
        };

        let media = groups.entry(date.clone()).or_insert_with(|| MediaEntry {
            date:    date.clone(),
            epub:    None,
            tldr:    None,
            radio:   None,
            podcast: None,
        });

        if file_name.starts_with("daily-news-") && file_name.ends_with(".epub") {
            // The -x4 variant (images stripped) is OPDS-only; the dashboard
            // download always gets the full edition
            if !file_name.ends_with("-x4.epub") {
                media.epub = Some(file_name);
            }
        } else if file_name.starts_with("daily-tldr-") && file_name.ends_with(".epub") {
            media.tldr = Some(file_name);
        } else if file_name.starts_with("short-radio-") && file_name.ends_with(".mp3") {
            media.radio = Some(file_name);
        } else if file_name.starts_with("long-podcast-") && file_name.ends_with(".mp3") {
            media.podcast = Some(file_name);
        }
    }

    // Collect and reverse so newest dates appear first
    let mut entries: Vec<MediaEntry> = groups.into_values().collect();
    entries.reverse();
    entries
}

// ═══════════════════════════════════════════════════════════════════
// ROUTE HANDLERS
// ═══════════════════════════════════════════════════════════════════

/// Version information response.
#[derive(Debug, Serialize, Deserialize)]
struct VersionResponse {
    version: String,
    date: String,
    git_sha: String,
    build_date: String,
}

/// `GET /api/version` — return the running image's build identity.
///
/// `git_sha` / `build_date` are baked in at image build time via
/// `--build-arg` (see build-image.yml); "dev"/"unknown" when running
/// outside that pipeline (e.g. `cargo run` locally).
async fn handle_version() -> Json<VersionResponse> {
    let date = Utc::now().format("%Y-%m-%d").to_string();
    Json(VersionResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        date,
        git_sha: std::env::var("GIT_SHA").unwrap_or_else(|_| "dev".to_string()),
        build_date: std::env::var("BUILD_DATE").unwrap_or_else(|_| "unknown".to_string()),
    })
}

/// `GET /api/media` — return a JSON list of all available media grouped by date.
async fn handle_list_media(
    State(state): State<AppState>,
) -> Result<Json<MediaListResponse>, StatusCode> {
    let dates = list_media_files(&state.data_dir);
    Ok(Json(MediaListResponse { dates }))
}

/// Minimal XML entity escaping for text nodes and attribute values.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// `GET /opds` — OPDS 1.2 acquisition feed of every generated EPUB, newest
/// first, so e-readers (the X4's Crosspoint OPDS browser, KOReader, etc.)
/// can pull books themselves instead of us pushing files to the device.
///
/// No auth for now — the station lives on a trusted home network. If that
/// ever changes, wrap this route and /media in basic-auth middleware.
async fn handle_opds(State(state): State<AppState>) -> Result<impl IntoResponse, StatusCode> {
    // Honour the Settings toggle — read fresh each request so flipping it
    // takes effect immediately, no restart needed.
    let config_path = state.data_dir.join("config.json");
    let enabled = std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|s| serde_json::from_str::<AppConfig>(&s).ok())
        .map(|c| c.opds_enabled)
        .unwrap_or(true);
    if !enabled {
        return Err(StatusCode::NOT_FOUND);
    }

    // (filename, modified) for every epub in the data dir
    let mut books: Vec<(String, DateTime<Utc>)> = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(&*state.data_dir) {
        for entry in read_dir.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            if !file_name.ends_with(".epub") {
                continue;
            }
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(DateTime::<Utc>::from)
                .unwrap_or_else(Utc::now);
            books.push((file_name, mtime));
        }
    }
    // E-readers are offline: prefer the -x4 variant (images stripped) and
    // hide the full edition when its -x4 twin exists. Editions predating the
    // variant split still appear as themselves.
    let names: std::collections::HashSet<String> =
        books.iter().map(|(n, _)| n.clone()).collect();
    books.retain(|(n, _)| {
        n.ends_with("-x4.epub")
            || !names.contains(&n.replace(".epub", "-x4.epub"))
    });
    books.sort_by(|a, b| b.1.cmp(&a.1)); // newest first

    let feed_updated = books
        .first()
        .map(|(_, m)| *m)
        .unwrap_or_else(Utc::now)
        .to_rfc3339();

    let date_re = Regex::new(r"\d{8}").expect("Invalid date regex");
    let mut entries = String::new();
    for (file_name, mtime) in &books {
        // Compact titles — e-reader lists truncate long ones, and the date
        // is the part that matters: yymmdd-news-ai / yymmdd-newsTLDR-ai
        let title = match date_re.find(file_name) {
            Some(m) => {
                let yymmdd = &m.as_str()[2..];
                if file_name.starts_with("daily-tldr-") {
                    format!("{}-newsTLDR-ai", yymmdd)
                } else {
                    format!("{}-news-ai", yymmdd)
                }
            }
            None => file_name.clone(),
        };
        // No per-entry <author>: CrossPoint names downloads
        // "{author} - {title}.epub", so an author string just bloats the
        // filename. The feed-level <author> below keeps the Atom feed valid.
        entries.push_str(&format!(
            r#"  <entry>
    <id>urn:my-news-station:{id}</id>
    <title>{title}</title>
    <updated>{updated}</updated>
    <link rel="http://opds-spec.org/acquisition" href="/media/{href}" type="application/epub+zip"/>
  </entry>
"#,
            id = xml_escape(file_name),
            title = xml_escape(&title),
            updated = mtime.to_rfc3339(),
            href = xml_escape(file_name),
        ));
    }

    let feed = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:opds="http://opds-spec.org/2010/catalog">
  <id>urn:my-news-station:catalog</id>
  <title>My News Station</title>
  <updated>{feed_updated}</updated>
  <author><name>AI News Station</name></author>
  <link rel="self" href="/opds" type="application/atom+xml;profile=opds-catalog;kind=acquisition"/>
  <link rel="start" href="/opds" type="application/atom+xml;profile=opds-catalog;kind=acquisition"/>
{entries}</feed>
"#
    );

    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "application/atom+xml;profile=opds-catalog;kind=acquisition",
        )],
        feed,
    ))
}

/// `GET /api/config` — read the current sources configuration from config.json or return defaults.
async fn handle_get_config(
    State(state): State<AppState>,
) -> Json<AppConfig> {
    let path = state.data_dir.join("config.json");
    let mut config = AppConfig::default();
    if path.exists() {
        match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<AppConfig>(&content) {
                Ok(parsed) => config = parsed,
                Err(e) => warn!(
                    "config.json is corrupt — serving defaults (file NOT overwritten; \
                     a Save from the UI will replace it): {}",
                    e
                ),
            },
            Err(e) => warn!("Cannot read config.json — serving defaults: {}", e),
        }
    }

    // Show the settings that are actually in force, so the UI does not display
    // an empty topic while the environment is driving real notifications.
    apply_ntfy_env_overrides(&mut config);

    // Tell the UI which fields it must not let the user edit: the environment
    // wins on every read, so an edit here would save and then silently revert.
    config.ntfy_env_locked = [
        ("NTFY_SERVER", "server"),
        ("NTFY_TOPIC", "topic"),
        ("NTFY_TOKEN", "token"),
        ("NTFY_ENABLED", "enabled"),
    ]
    .iter()
    .filter(|(var, _)| std::env::var(var).is_ok_and(|v| !v.trim().is_empty()))
    .map(|(_, field)| field.to_string())
    .collect();

    // Never hand an environment-supplied token to a browser: it comes from the
    // secret store, not from this user, and config.json is world-readable to
    // anyone who can reach the dashboard.
    if std::env::var("NTFY_TOKEN").is_ok_and(|v| !v.trim().is_empty()) {
        config.ntfy_token = None;
    }

    Json(config)
}

/// `POST /api/config` — save the new sources configuration to config.json.
///
/// Merges onto the existing on-disk config at the JSON-object level rather
/// than trusting the payload as the complete state: almost every AppConfig
/// field has `#[serde(default)]`, so a payload missing a field (a stale
/// browser tab, a partial save from one settings panel) would otherwise
/// silently reset that field to its zero value and wipe it on write.
async fn handle_post_config(
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, StatusCode> {
    let path = state.data_dir.join("config.json");

    let existing = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    } else {
        None
    };
    let mut merged = existing.unwrap_or_else(|| {
        serde_json::to_value(AppConfig::default()).expect("AppConfig serializes")
    });

    match (merged.as_object_mut(), payload.as_object()) {
        (Some(merged_obj), Some(payload_obj)) => {
            for (k, v) in payload_obj {
                merged_obj.insert(k.clone(), v.clone());
            }
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    }

    // Validate the merged result still deserializes as AppConfig before
    // committing it to disk — catches typos/bad types in the payload.
    let validated: AppConfig =
        serde_json::from_value(merged).map_err(|_| StatusCode::BAD_REQUEST)?;

    if let Ok(content) = serde_json::to_string_pretty(&validated) {
        if std::fs::write(&path, content).is_ok() {
            info!("Successfully saved new configuration to {:?}", path);
            return Ok(StatusCode::OK);
        }
    }
    Err(StatusCode::INTERNAL_SERVER_ERROR)
}

/// `GET /api/sources/activity` — read and return source activity (last seen dates) from data/source_activity.json
async fn handle_get_source_activity(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let path = state.data_dir.join("source_activity.json");
    if path.exists() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(activity) = serde_json::from_str::<serde_json::Value>(&content) {
                return Ok(Json(activity));
            }
        }
    }
    // Return empty object if no activity tracking exists yet
    Ok(Json(serde_json::json!({})))
}

#[derive(Serialize)]
struct ScrapeStatus {
    running: bool,
    last_run_success: bool,
}

/// `GET /api/scrape/status` — returns whether the scraper is currently running.
async fn handle_scrape_status(
    State(state): State<AppState>,
) -> Json<ScrapeStatus> {
    Json(ScrapeStatus {
        running: state.is_scraping.load(Ordering::SeqCst),
        last_run_success: state.last_run_success.load(Ordering::SeqCst),
    })
}

/// `GET /api/scrape/logs` — returns the current array of log lines.
async fn handle_scrape_logs(
    State(state): State<AppState>,
) -> Json<Vec<String>> {
    let logs = state.scraper_logs.lock().await;
    Json(logs.iter().cloned().collect())
}

#[derive(Deserialize)]
struct TriggerParams {
    voice_short: Option<String>,
    voice_long: Option<String>,
    short_sources: Option<String>,
    long_sources: Option<String>,
}

// ═══════════════════════════════════════════════════════════════════
// NTFY PUSH NOTIFICATIONS
// ═══════════════════════════════════════════════════════════════════

/// One publishable notification: ntfy maps these onto its title/priority/tags
/// headers, which is what drives the phone's banner, icon and alert sound.
struct NtfyMessage {
    title: String,
    body: String,
    /// ntfy priority 1 (min) … 5 (max).
    priority: u8,
    /// ntfy tag names — emoji shortcodes render as the notification icon.
    tags: &'static str,
}

/// Log-line fragments that mean "the LLM credentials are expired or rejected"
/// rather than "the run broke". Matched lowercase against the run's captured
/// output. Deliberately specific: a bare "401" also appears in scraped article
/// text, which would misfire the credential alert on an unrelated failure.
const TOKEN_EXPIRY_MARKERS: &[&str] = &[
    "authentication_error",
    "invalid x-api-key",
    "invalid api key",
    "api key expired",
    "oauth token has expired",
    "oauth token expired",
    "token has expired",
    "401 unauthorized",
    "status 401",
    "error 401",
    "please run /login",
    "credit balance is too low",
    "anthropic_api_key is not set",
    "google_ai_key is not set",
];

/// True if the run's output contains a credential-failure marker.
fn looks_like_token_expiry(lines: &[String]) -> bool {
    lines.iter().any(|line| {
        let lower = line.to_lowercase();
        TOKEN_EXPIRY_MARKERS.iter().any(|m| lower.contains(m))
    })
}

/// Pull out the log lines worth putting in a failure notification: the stderr
/// lines and anything logged at ERROR, capped so the push stays readable on a
/// lock screen.
fn failure_excerpt(lines: &[String]) -> String {
    const MAX_LINES: usize = 12;
    const MAX_CHARS: usize = 1500;

    let interesting: Vec<&String> = lines
        .iter()
        .filter(|l| {
            let lower = l.to_lowercase();
            l.starts_with("[ERROR] ")
                || lower.contains("[error]")
                || lower.contains("traceback")
                || lower.contains("error:")
        })
        .collect();

    // Nothing matched the filters (e.g. the process died without logging) —
    // fall back to the tail of the run, which is where the failure will be.
    let chosen: Vec<&String> = if interesting.is_empty() {
        lines.iter().rev().take(MAX_LINES).rev().collect()
    } else {
        interesting.into_iter().rev().take(MAX_LINES).rev().collect()
    };

    let mut out = chosen
        .iter()
        .map(|l| l.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    if out.chars().count() > MAX_CHARS {
        out = out.chars().take(MAX_CHARS).collect::<String>() + "\n…(truncated)";
    }
    if out.trim().is_empty() {
        out = "(no output captured)".to_string();
    }
    out
}

/// POST a message to an ntfy topic. Returns the server's error text on failure
/// so the Settings "Send test" button can show why it didn't work.
///
/// Never called when notifications are disabled — the caller gates on config.
async fn ntfy_publish(
    server: &str,
    topic: &str,
    token: Option<&str>,
    msg: &NtfyMessage,
) -> Result<(), String> {
    let topic = topic.trim();
    if topic.is_empty() {
        return Err("No ntfy topic configured".to_string());
    }
    let url = format!("{}/{}", server.trim().trim_end_matches('/'), topic);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Cannot build HTTP client: {}", e))?;

    let mut req = client
        .post(&url)
        .header("Title", &msg.title)
        .header("Priority", msg.priority.to_string())
        .header("Tags", msg.tags)
        .body(msg.body.clone());

    if let Some(t) = token.map(str::trim).filter(|t| !t.is_empty()) {
        req = req.bearer_auth(t);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("Request to {} failed: {}", url, e))?;

    if resp.status().is_success() {
        return Ok(());
    }

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    Err(format!(
        "ntfy returned {}: {}",
        status,
        body.chars().take(300).collect::<String>()
    ))
}

/// Publish using the saved config, logging rather than propagating failures —
/// a dead ntfy server must never take the pipeline down with it.
async fn notify(data_dir: &std::path::Path, msg: NtfyMessage) {
    let cfg = load_app_config(data_dir);
    if !cfg.ntfy_enabled {
        return;
    }
    if let Err(e) = ntfy_publish(
        &cfg.ntfy_server,
        &cfg.ntfy_topic,
        cfg.ntfy_token.as_deref(),
        &msg,
    )
    .await
    {
        warn!("ntfy notification failed: {}", e);
    }
}

/// Body of the `POST /api/ntfy/test` request. Carries the values currently in
/// the Settings form, so the user can verify a connection before saving it.
/// Every field is optional so the UI can omit the ones the environment owns:
/// those inputs are locked and (for the token) never populated, so sending them
/// would test a blank credential and report a failure that does not reflect how
/// real notifications are sent.
#[derive(Deserialize)]
struct NtfyTestRequest {
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    token: Option<String>,
}

#[derive(Serialize)]
struct NtfyTestResponse {
    ok: bool,
    error: Option<String>,
}

/// `POST /api/ntfy/test` — send a one-off test notification.
async fn handle_ntfy_test(
    State(state): State<AppState>,
    Json(req): Json<NtfyTestRequest>,
) -> Json<NtfyTestResponse> {
    // Fall back to the settings actually in force for anything the form did not
    // supply, so a test exercises the same values a real notification would.
    let effective = load_app_config(&state.data_dir);
    let non_empty = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

    let server = non_empty(req.server).unwrap_or(effective.ntfy_server);
    let topic = non_empty(req.topic).unwrap_or(effective.ntfy_topic);
    let token = non_empty(req.token).or(effective.ntfy_token);

    let msg = NtfyMessage {
        title: "Daily News Station".to_string(),
        body: "Test notification — ntfy is wired up correctly.".to_string(),
        priority: 3,
        tags: "newspaper",
    };
    match ntfy_publish(&server, &topic, token.as_deref(), &msg).await {
        Ok(()) => Json(NtfyTestResponse { ok: true, error: None }),
        Err(e) => {
            warn!("ntfy test failed: {}", e);
            Json(NtfyTestResponse { ok: false, error: Some(e) })
        }
    }
}

/// Pump one child stream into the shared log ring buffer, line by line.
fn pump_child_stream<R>(
    stream: Option<R>,
    logs: Arc<tokio::sync::Mutex<std::collections::VecDeque<String>>>,
    prefix: &'static str,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    const MAX_LOG_LINES: usize = 150;
    let Some(stream) = stream else { return };
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut reader = BufReader::new(stream).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let mut l = logs.lock().await;
            l.push_back(format!("{}{}", prefix, line));
            if l.len() > MAX_LOG_LINES {
                l.pop_front();
            }
        }
    });
}

/// Spawn the Python scraper as a background job with the given env vars,
/// streaming its output into the log ring buffer and tracking success.
///
/// What kicked off a scraper run. Only used to decide which ntfy "started"
/// toggle applies — scheduled and manual runs are opt-in separately, since a
/// manual run's result is already visible on screen.
#[derive(Clone, Copy, PartialEq)]
enum RunTrigger {
    Scheduled,
    Manual,
}

/// The caller must have already claimed the `is_scraping` flag; this function
/// releases it when the child exits. `label` names the job in log lines
/// ("Pipeline", "Audio regen").
fn spawn_scraper_job(
    state: AppState,
    envs: Vec<(&'static str, String)>,
    label: &'static str,
    trigger: RunTrigger,
) {
    let python_bin = std::env::var("PYTHON_BIN").unwrap_or_else(|_| "python3".to_string());
    let scraper_script =
        std::env::var("SCRAPER_SCRIPT").unwrap_or_else(|_| "scraper/scraper.py".to_string());

    tokio::spawn(async move {
        info!(
            "Spawning background {} process: {} {}",
            label, python_bin, scraper_script
        );

        let mut cmd = tokio::process::Command::new(&python_bin);
        cmd.arg(&scraper_script);
        for (key, value) in envs {
            cmd.env(key, value);
        }

        // Pipe stdout & stderr to capture logs in real time
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        // Clear logs at start of a new run
        {
            let mut logs = state.scraper_logs.lock().await;
            logs.clear();
            logs.push_back(format!("--- Starting {} ---", label));
        }
        state.last_run_success.store(true, Ordering::SeqCst);

        let cfg = load_app_config(&state.data_dir);
        let announce_start = match trigger {
            RunTrigger::Scheduled => cfg.ntfy_on_scheduled_run,
            RunTrigger::Manual => cfg.ntfy_on_manual_run,
        };
        if announce_start {
            notify(
                &state.data_dir,
                NtfyMessage {
                    title: "News run started".to_string(),
                    body: format!(
                        "{} started at {} UTC.",
                        label,
                        Utc::now().format("%Y-%m-%d %H:%M")
                    ),
                    priority: 2,
                    tags: "hourglass_flowing_sand",
                },
            )
            .await;
        }

        match cmd.spawn() {
            Ok(mut child) => {
                pump_child_stream(child.stdout.take(), Arc::clone(&state.scraper_logs), "");
                pump_child_stream(child.stderr.take(), Arc::clone(&state.scraper_logs), "[ERROR] ");

                match child.wait().await {
                    Ok(status) => {
                        info!("Background {} completed with status: {:?}", label, status);
                        let success = status.success();
                        state.last_run_success.store(success, Ordering::SeqCst);
                        let mut l = state.scraper_logs.lock().await;
                        if success {
                            l.push_back(format!("--- {} completed successfully ---", label));
                        } else {
                            l.push_back(format!(
                                "--- {} failed with exit status: {:?} ---",
                                label, status
                            ));
                        }
                    }
                    Err(e) => {
                        error!("Failed to wait for background {} process: {}", label, e);
                        state.last_run_success.store(false, Ordering::SeqCst);
                        let mut l = state.scraper_logs.lock().await;
                        l.push_back(format!("--- Error waiting for {}: {} ---", label, e));
                    }
                }
            }
            Err(e) => {
                error!(
                    "Failed to spawn background {} process ({} {}): {}",
                    label, python_bin, scraper_script, e
                );
                state.last_run_success.store(false, Ordering::SeqCst);
                let mut l = state.scraper_logs.lock().await;
                l.push_back(format!("--- Failed to spawn {} process: {} ---", label, e));
            }
        }

        // Let the stdout/stderr pump tasks drain the last few lines before the
        // notification snapshots them — the child exiting closes the pipes but
        // does not mean those tasks have finished writing to the ring buffer.
        time::sleep(time::Duration::from_millis(250)).await;

        let success = state.last_run_success.load(Ordering::SeqCst);
        let cfg = load_app_config(&state.data_dir);
        if cfg.ntfy_enabled {
            let lines: Vec<String> = state.scraper_logs.lock().await.iter().cloned().collect();
            let msg = if success {
                cfg.ntfy_on_success.then(|| NtfyMessage {
                    title: "News run complete".to_string(),
                    body: format!("{} finished successfully.", label),
                    priority: 2,
                    tags: "white_check_mark",
                })
            } else if looks_like_token_expiry(&lines) && cfg.ntfy_on_token_expiry {
                // Credentials, not a code fault — worth a louder alert, since
                // every run stays broken until someone re-authenticates.
                Some(NtfyMessage {
                    title: "LLM credentials rejected".to_string(),
                    body: format!(
                        "{} failed: the API key or OAuth token looks expired or invalid.\n\n{}",
                        label,
                        failure_excerpt(&lines)
                    ),
                    priority: 5,
                    tags: "key,rotating_light",
                })
            } else {
                cfg.ntfy_on_failure.then(|| NtfyMessage {
                    title: "News run failed".to_string(),
                    body: format!("{} failed.\n\n{}", label, failure_excerpt(&lines)),
                    priority: 4,
                    tags: "rotating_light",
                })
            };
            if let Some(msg) = msg {
                notify(&state.data_dir, msg).await;
            }
        }

        state.is_scraping.store(false, Ordering::SeqCst);
    });
}

/// `POST /api/scrape/trigger` — spawns the Python scraper script in the background.
async fn handle_scrape_trigger(
    State(state): State<AppState>,
    Query(params): Query<TriggerParams>,
) -> Result<Json<ScrapeStatus>, StatusCode> {
    let was_running = state.is_scraping.swap(true, Ordering::SeqCst);
    if was_running {
        return Err(StatusCode::CONFLICT);
    }

    let mut envs: Vec<(&'static str, String)> = Vec::new();
    if let Some(vs) = params.voice_short { envs.push(("VOICE_SHORT", vs)); }
    if let Some(vl) = params.voice_long { envs.push(("VOICE_LONG", vl)); }
    if let Some(ss) = params.short_sources { envs.push(("SHORT_SOURCES", ss)); }
    if let Some(ls) = params.long_sources { envs.push(("LONG_SOURCES", ls)); }

    trigger_scrape(state.clone(), envs, "news scraper pipeline", RunTrigger::Manual);

    Ok(Json(ScrapeStatus {
        running: true,
        last_run_success: state.last_run_success.load(Ordering::SeqCst),
    }))
}

/// `POST /api/scrape/regen-audio?date=DATESTR` — re-runs only LLM + TTS for an existing date,
/// loading the saved articles sidecar. No full scrape performed.
#[derive(Deserialize)]
struct RegenAudioParams {
    date: Option<String>,
    /// What to regenerate: "radio" | "podcast" (that audio track only),
    /// "epub" (rebuild book from saved articles, no LLM), or "tldr"
    /// (re-summarize the digest only). Absent = both tracks + TLDR.
    track: Option<String>,
    voice_short: Option<String>,
    voice_long: Option<String>,
    short_sources: Option<String>,
    long_sources: Option<String>,
}

async fn handle_regen_audio(
    State(state): State<AppState>,
    Query(params): Query<RegenAudioParams>,
) -> Result<Json<ScrapeStatus>, StatusCode> {
    let date_str = params.date.clone().ok_or(StatusCode::BAD_REQUEST)?;

    let was_running = state.is_scraping.swap(true, Ordering::SeqCst);
    if was_running {
        return Err(StatusCode::CONFLICT);
    }

    let mut envs: Vec<(&'static str, String)> = vec![("REGEN_DATE", date_str)];
    if let Some(t) = params.track {
        if matches!(t.as_str(), "radio" | "podcast" | "epub" | "tldr") {
            envs.push(("REGEN_TRACK", t));
        }
    }
    if let Some(vs) = params.voice_short { envs.push(("VOICE_SHORT", vs)); }
    if let Some(vl) = params.voice_long { envs.push(("VOICE_LONG", vl)); }
    if let Some(ss) = params.short_sources { envs.push(("SHORT_SOURCES", ss)); }
    if let Some(ls) = params.long_sources { envs.push(("LONG_SOURCES", ls)); }

    trigger_scrape(state.clone(), envs, "audio regen", RunTrigger::Manual);

    Ok(Json(ScrapeStatus {
        running: true,
        last_run_success: state.last_run_success.load(Ordering::SeqCst),
    }))
}

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
static PREVIEW_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Deserialize)]
struct PreviewParams {
    voice: String,
}

/// `GET /api/tts/preview` — generates a short voice sample using edge-tts and streams the audio.
async fn handle_tts_preview(
    Query(params): Query<PreviewParams>,
) -> impl IntoResponse {
    let voice = params.voice;
    // Security check: only allow alphanumeric, hyphens, and underscores in voice name
    if !voice.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return (StatusCode::BAD_REQUEST, "Invalid voice identifier").into_response();
    }

    // Try to extract a clean name for a friendly prefix, e.g. "Sonia" or "Guy"
    let clean_name = voice.split('-').last().unwrap_or(&voice).replace("Neural", "");
    let text = format!("Hello! This is a preview of the {} voice.", clean_name);

    let count = PREVIEW_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    
    let filename = format!("preview-{}-{}.mp3", timestamp, count);
    let preview_path = std::env::temp_dir().join(filename);

    info!("Generating TTS preview for voice: {} -> {:?}", voice, preview_path);

    // Try spawning edge-tts command, fall back to python3 -m edge_tts if it fails
    let mut success = false;
    let mut cmd = tokio::process::Command::new("edge-tts");
    cmd.arg("--voice").arg(&voice)
       .arg("--text").arg(&text)
       .arg("--write-media").arg(&preview_path);

    match cmd.spawn() {
        Ok(mut child) => {
            if let Ok(status) = child.wait().await {
                if status.success() {
                    success = true;
                }
            }
        }
        Err(_) => {}
    }

    if !success {
        // Fallback to calling python3 -m edge_tts
        info!("edge-tts direct execution failed, attempting fallback python3 -m edge_tts");
        let mut fallback_cmd = tokio::process::Command::new("python3");
        fallback_cmd.arg("-m").arg("edge_tts.cli")
           .arg("--voice").arg(&voice)
           .arg("--text").arg(&text)
           .arg("--write-media").arg(&preview_path);

        if let Ok(mut child) = fallback_cmd.spawn() {
            if let Ok(status) = child.wait().await {
                if status.success() {
                    success = true;
                }
            }
        }
    }

    if success {
        // Read file
        match tokio::fs::read(&preview_path).await {
            Ok(bytes) => {
                // Clean up file asynchronously
                let _ = tokio::fs::remove_file(&preview_path).await;
                return (
                    [(axum::http::header::CONTENT_TYPE, "audio/mpeg")],
                    bytes,
                ).into_response();
            }
            Err(e) => {
                error!("Failed to read preview file: {}", e);
            }
        }
    } else {
        error!("Both edge-tts and fallback python3 -m edge_tts failed to generate preview");
    }

    // Clean up if it was created but not read
    let _ = tokio::fs::remove_file(&preview_path).await;
    (StatusCode::INTERNAL_SERVER_ERROR, "Failed to generate preview").into_response()
}


// ═══════════════════════════════════════════════════════════════════
// BACKGROUND CLEANUP TASK
// ═══════════════════════════════════════════════════════════════════

/// Delete any regular file in `data_dir` whose mtime is older than `max_age_days`.
async fn cleanup_old_files(data_dir: &Path, max_age_days: i64) {
    let cutoff: DateTime<Utc> = Utc::now() - Duration::days(max_age_days);
    info!(
        "Running storage cleanup — removing files older than {} days (cutoff: {})",
        max_age_days,
        cutoff.format("%Y-%m-%d %H:%M UTC")
    );

    let read_dir = match std::fs::read_dir(data_dir) {
        Ok(rd) => rd,
        Err(e) => {
            warn!("Cleanup: cannot read {:?}: {}", data_dir, e);
            return;
        }
    };

    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let modified: DateTime<Utc> = match entry.metadata().and_then(|m| m.modified()) {
            Ok(sys_time) => sys_time.into(),
            Err(e) => {
                warn!("Cleanup: could not read mtime for {:?}: {}", path, e);
                continue;
            }
        };

        if modified < cutoff {
            match std::fs::remove_file(&path) {
                Ok(_)  => info!("Cleanup: deleted {:?} (mtime {})", path, modified.format("%Y-%m-%d")),
                Err(e) => error!("Cleanup: failed to delete {:?}: {}", path, e),
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// KUBERNETES JOB RUNNER  (active when SCRAPE_RUNNER=k8s)
//
// When deployed in Kubernetes the server must NOT fork Playwright in its
// own pod: a 512Mi server pod cannot host a Chromium process.  Setting
// SCRAPE_RUNNER=k8s makes the server create a Kubernetes Job from the
// existing CronJob's jobTemplate instead, which runs in the CronJob's
// properly sized pod (3Gi request / 6Gi limit).
//
// The internal daily scheduler is also disabled in this mode — the
// CronJob owns the schedule, keeping it declarative and in Git.
//
// SCRAPE_RUNNER=local (default) preserves the original fork behaviour so
// docker-compose users are unaffected.
// ═══════════════════════════════════════════════════════════════════

fn is_k8s_mode() -> bool {
    std::env::var("SCRAPE_RUNNER").as_deref() == Ok("k8s")
}

/// Namespace for Job creation: explicit env var first, then the
/// projected ServiceAccount file (present inside any k8s pod).
fn k8s_namespace() -> String {
    std::env::var("K8S_NAMESPACE")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::fs::read_to_string(
                "/var/run/secrets/kubernetes.io/serviceaccount/namespace",
            )
            .ok()
            .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "my-news".to_string())
}

/// Stream the logs of the first pod belonging to a Job into the log ring
/// buffer.  Waits up to 5 minutes for a pod to be scheduled, then up to
/// 2 minutes for it to leave Pending.  Silently returns on timeout so the
/// job status poller can still mark the run as failed if appropriate.
async fn stream_pod_logs_for_job(
    client: kube::Client,
    namespace: String,
    job_name: String,
    logs: Arc<tokio::sync::Mutex<std::collections::VecDeque<String>>>,
) {
    use futures::{io::AsyncBufReadExt as _, StreamExt as _};
    use k8s_openapi::api::core::v1::Pod;
    use kube::{api::ListParams, api::LogParams, Api};

    let pod_api: Api<Pod> = Api::namespaced(client, &namespace);
    let label = format!("batch.kubernetes.io/job-name={}", job_name);
    let lp = ListParams::default().labels(&label);

    /// Record why the run's output is missing, in the log buffer as well as the
    /// server log. Without this the UI console is silently blank and, worse,
    /// `failure_excerpt` has nothing to quote and `looks_like_token_expiry`
    /// cannot match — so an expired API key would alert as a generic failure.
    async fn report_unavailable(
        logs: &Arc<tokio::sync::Mutex<std::collections::VecDeque<String>>>,
        reason: String,
    ) {
        warn!("{}", reason);
        let mut l = logs.lock().await;
        l.push_back(format!("[ERROR] {}", reason));
    }

    // Wait up to 5 min (60 × 5 s) for the pod to be created.
    let pod_name = {
        let mut found: Option<String> = None;
        for _ in 0..60_u32 {
            time::sleep(time::Duration::from_secs(5)).await;
            match pod_api.list(&lp).await {
                Ok(list) => {
                    if let Some(name) = list.items.into_iter().find_map(|p| p.metadata.name) {
                        found = Some(name);
                        break;
                    }
                }
                // 403 will not fix itself by retrying for five minutes: the
                // ServiceAccount needs pods:list and pods/log:get in its Role.
                Err(kube::Error::Api(ref resp)) if resp.code == 403 => {
                    report_unavailable(&logs, format!(
                        "Cannot read scraper logs: the ServiceAccount is not allowed to list pods \
                         in this namespace (needs pods: list,get and pods/log: get). \
                         Run output will be missing from the dashboard and from failure notifications."
                    )).await;
                    return;
                }
                Err(e) => warn!("Waiting for pod for job {}: {}", job_name, e),
            }
        }
        match found {
            Some(n) => n,
            None => {
                report_unavailable(&logs, format!(
                    "No pod appeared for job {} within 5 minutes — no run output captured",
                    job_name
                )).await;
                return;
            }
        }
    };

    // Wait up to 2 min for the pod to leave Pending.
    for _ in 0..24_u32 {
        match pod_api.get_status(&pod_name).await {
            Ok(pod) => {
                let phase = pod.status.and_then(|s| s.phase).unwrap_or_default();
                if !phase.is_empty() && phase != "Pending" {
                    break;
                }
            }
            Err(_) => {}
        }
        time::sleep(time::Duration::from_secs(5)).await;
    }

    // Stream logs line-by-line into the ring buffer.
    // kube 4.x: log_stream() returns impl futures::AsyncBufRead, not a byte Stream.
    let log_params = LogParams { follow: true, timestamps: false, ..Default::default() };
    match pod_api.log_stream(&pod_name, &log_params).await {
        Ok(reader) => {
            const MAX_LOG_LINES: usize = 150;
            let mut lines = reader.lines();
            while let Some(result) = lines.next().await {
                match result {
                    Ok(line) => {
                        let mut l = logs.lock().await;
                        l.push_back(line);
                        if l.len() > MAX_LOG_LINES { l.pop_front(); }
                    }
                    Err(e) => {
                        warn!("Log stream error for pod {}: {}", pod_name, e);
                        break;
                    }
                }
            }
        }
        Err(e) => warn!("Could not stream logs for pod {}: {}", pod_name, e),
    }
}

/// Fetch the tail of a finished Job's pod logs in one shot (no follow).
///
/// The streaming path only starts when a Job is observed *running*, so a Job
/// that begins and ends between two polls — or one that was already running
/// when the server restarted — would otherwise leave no output at all. That
/// matters beyond the dashboard console: `failure_excerpt` would have nothing
/// to quote, and `looks_like_token_expiry` could not match, downgrading an
/// expired-credentials alert to a generic failure.
async fn fetch_job_logs_once(
    client: kube::Client,
    namespace: &str,
    job_name: &str,
    logs: &Arc<tokio::sync::Mutex<std::collections::VecDeque<String>>>,
) {
    use k8s_openapi::api::core::v1::Pod;
    use kube::{api::ListParams, api::LogParams, Api};

    let pod_api: Api<Pod> = Api::namespaced(client, namespace);
    let selector = format!("batch.kubernetes.io/job-name={}", job_name);

    let pods = match pod_api.list(&ListParams::default().labels(&selector)).await {
        Ok(list) => list.items,
        Err(e) => {
            warn!("Cannot list pods for finished job {}: {}", job_name, e);
            return;
        }
    };
    let Some(pod_name) = pods.into_iter().find_map(|p| p.metadata.name) else {
        warn!("No pod found for finished job {} — its pod may already be deleted", job_name);
        return;
    };

    let params = LogParams { tail_lines: Some(200), ..Default::default() };
    match pod_api.logs(&pod_name, &params).await {
        Ok(text) => {
            const MAX_LOG_LINES: usize = 150;
            let mut l = logs.lock().await;
            for line in text.lines() {
                l.push_back(line.to_string());
                if l.len() > MAX_LOG_LINES {
                    l.pop_front();
                }
            }
        }
        Err(e) => warn!("Cannot read logs for pod {}: {}", pod_name, e),
    }
}

/// Create a Kubernetes Job from the `news-scraper` CronJob's jobTemplate,
/// inject optional env vars, then poll until the Job completes.
///
/// Mirrors `spawn_scraper_job` in calling convention: spawns a background
/// task, releases `is_scraping` on completion, and fires ntfy notifications.
fn spawn_k8s_job(
    state: AppState,
    extra_envs: Vec<(&'static str, String)>,
    label: &'static str,
    trigger: RunTrigger,
) {
    use k8s_openapi::api::batch::v1::{CronJob, Job};
    use k8s_openapi::api::core::v1::EnvVar;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use kube::{api::PostParams, Api};

    let namespace = k8s_namespace();
    let cronjob_name =
        std::env::var("K8S_CRONJOB_NAME").unwrap_or_else(|_| "news-scraper".to_string());
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let job_prefix = if label.contains("regen") { "news-scraper-regen" } else { "news-scraper-manual" };
    let job_name = format!("{}-{}", job_prefix, ts);

    tokio::spawn(async move {
        // Clear log buffer for this run.
        {
            let mut l = state.scraper_logs.lock().await;
            l.clear();
            l.push_back(format!("--- Starting {} (k8s mode) ---", label));
        }
        state.last_run_success.store(true, Ordering::SeqCst);

        // ── Build the k8s client ──────────────────────────────────────
        let client = match kube::Client::try_default().await {
            Ok(c) => c,
            Err(e) => {
                let msg = format!("Cannot connect to Kubernetes API: {}", e);
                error!("{}", msg);
                let mut l = state.scraper_logs.lock().await;
                l.push_back(format!("[ERROR] {}", msg));
                state.last_run_success.store(false, Ordering::SeqCst);
                state.is_scraping.store(false, Ordering::SeqCst);
                return;
            }
        };

        // ── Fetch CronJob → extract jobTemplate.spec ──────────────────
        let cj_api: Api<CronJob> = Api::namespaced(client.clone(), &namespace);
        let mut job_spec = match cj_api.get(&cronjob_name).await {
            Ok(cj) => match cj.spec.and_then(|s| s.job_template.spec) {
                Some(js) => js,
                None => {
                    let msg = format!("CronJob {} has no jobTemplate.spec", cronjob_name);
                    error!("{}", msg);
                    let mut l = state.scraper_logs.lock().await;
                    l.push_back(format!("[ERROR] {}", msg));
                    state.last_run_success.store(false, Ordering::SeqCst);
                    state.is_scraping.store(false, Ordering::SeqCst);
                    return;
                }
            },
            Err(e) => {
                let msg = format!("Cannot get CronJob {}: {}", cronjob_name, e);
                error!("{}", msg);
                let mut l = state.scraper_logs.lock().await;
                l.push_back(format!("[ERROR] {}", msg));
                state.last_run_success.store(false, Ordering::SeqCst);
                state.is_scraping.store(false, Ordering::SeqCst);
                return;
            }
        };

        // ── Inject extra env vars into every container ─────────────────
        // For audio regen: REGEN_DATE, REGEN_TRACK etc are passed this way.
        if !extra_envs.is_empty() {
            let add: Vec<EnvVar> = extra_envs.iter().map(|(k, v)| EnvVar {
                name: k.to_string(),
                value: Some(v.clone()),
                ..Default::default()
            }).collect();
            if let Some(pod_spec) = job_spec.template.spec.as_mut() {
                for c in pod_spec.containers.iter_mut() {
                    let mut envs = c.env.take().unwrap_or_default();
                    for e in &add {
                        envs.retain(|x| x.name != e.name);
                        envs.push(e.clone());
                    }
                    c.env = Some(envs);
                }
            }
        }

        // ── Create the Job ─────────────────────────────────────────────
        let job = Job {
            metadata: ObjectMeta {
                name: Some(job_name.clone()),
                namespace: Some(namespace.clone()),
                ..Default::default()
            },
            spec: Some(job_spec),
            ..Default::default()
        };
        let job_api: Api<Job> = Api::namespaced(client.clone(), &namespace);
        if let Err(e) = job_api.create(&PostParams::default(), &job).await {
            let msg = format!("Failed to create Job {}: {}", job_name, e);
            error!("{}", msg);
            let mut l = state.scraper_logs.lock().await;
            l.push_back(format!("[ERROR] {}", msg));
            state.last_run_success.store(false, Ordering::SeqCst);
            state.is_scraping.store(false, Ordering::SeqCst);
            return;
        }
        {
            let mut l = state.scraper_logs.lock().await;
            l.push_back(format!("--- Kubernetes Job {} created ---", job_name));
        }
        info!("Created Kubernetes Job {} for {}", job_name, label);

        // ── ntfy: run started ──────────────────────────────────────────
        let cfg = load_app_config(&state.data_dir);
        let announce_start = match trigger {
            RunTrigger::Scheduled => cfg.ntfy_on_scheduled_run,
            RunTrigger::Manual    => cfg.ntfy_on_manual_run,
        };
        if announce_start {
            notify(&state.data_dir, NtfyMessage {
                title: "News run started".to_string(),
                body: format!(
                    "{} started at {} UTC (job: {}).",
                    label, Utc::now().format("%Y-%m-%d %H:%M"), job_name
                ),
                priority: 2,
                tags: "hourglass_flowing_sand",
            }).await;
        }

        // ── Stream pod logs in a background task ───────────────────────
        tokio::spawn(stream_pod_logs_for_job(
            client.clone(),
            namespace.clone(),
            job_name.clone(),
            Arc::clone(&state.scraper_logs),
        ));

        // ── Poll job status until it completes ─────────────────────────
        let success = loop {
            time::sleep(time::Duration::from_secs(15)).await;

            let status = match job_api.get_status(&job_name).await {
                Ok(j) => j.status.unwrap_or_default(),
                Err(kube::Error::Api(ref resp)) if resp.code == 404 => {
                    // Cleaned up by ttlSecondsAfterFinished before we polled.
                    info!("Job {} no longer exists (TTL cleanup) — ending poll", job_name);
                    break state.last_run_success.load(Ordering::SeqCst);
                }
                Err(e) => {
                    warn!("Error polling Job {}: {}", job_name, e);
                    continue;
                }
            };

            let succeeded = status.succeeded.unwrap_or(0);
            let failed    = status.failed.unwrap_or(0);
            let active    = status.active.unwrap_or(0);

            if succeeded > 0 {
                info!("Job {} succeeded", job_name);
                break true;
            }
            if failed > 0 && active == 0 {
                // backoffLimit exhausted — no more pods will be started.
                info!("Job {} failed (failed={}, active={})", job_name, failed, active);
                break false;
            }
        };

        state.last_run_success.store(success, Ordering::SeqCst);
        {
            let mut l = state.scraper_logs.lock().await;
            if success {
                l.push_back(format!("--- {} completed successfully ---", label));
            } else {
                l.push_back(format!("--- {} failed ---", label));
            }
        }

        // ── ntfy: completion ───────────────────────────────────────────
        time::sleep(time::Duration::from_millis(250)).await;
        let cfg = load_app_config(&state.data_dir);
        if cfg.ntfy_enabled {
            let lines: Vec<String> = state.scraper_logs.lock().await.iter().cloned().collect();
            let msg = if success {
                cfg.ntfy_on_success.then(|| NtfyMessage {
                    title: "News run complete".to_string(),
                    body: format!("{} finished successfully.", label),
                    priority: 2,
                    tags: "white_check_mark",
                })
            } else if looks_like_token_expiry(&lines) && cfg.ntfy_on_token_expiry {
                Some(NtfyMessage {
                    title: "LLM credentials rejected".to_string(),
                    body: format!(
                        "{} failed: the API key or OAuth token looks expired or invalid.\n\n{}",
                        label, failure_excerpt(&lines)
                    ),
                    priority: 5,
                    tags: "key,rotating_light",
                })
            } else {
                cfg.ntfy_on_failure.then(|| NtfyMessage {
                    title: "News run failed".to_string(),
                    body: format!("{} failed.\n\n{}", label, failure_excerpt(&lines)),
                    priority: 4,
                    tags: "rotating_light",
                })
            };
            if let Some(msg) = msg {
                notify(&state.data_dir, msg).await;
            }
        }

        state.is_scraping.store(false, Ordering::SeqCst);
    });
}

/// Watch Jobs created by the CronJob and report their outcome over ntfy.
///
/// Under `SCRAPE_RUNNER=k8s` the daily run is a pod the CronJob creates — the
/// server neither spawns it nor is told about it, so without this loop the
/// unattended 06:00 run is the one run that never notifies. All ntfy code lives
/// in this binary (the scraper has none), so the server has to observe those
/// Jobs itself.
///
/// CronJob-created Jobs carry an ownerReference back to the CronJob; Jobs this
/// server creates do not. That is the discriminator, so the two paths cannot
/// double-notify for the same run.
async fn cronjob_watcher_loop(state: AppState) {
    use k8s_openapi::api::batch::v1::Job;
    use kube::{api::ListParams, Api};
    use std::collections::HashSet;

    if !is_k8s_mode() {
        return;
    }

    let namespace = k8s_namespace();
    let cronjob_name =
        std::env::var("K8S_CRONJOB_NAME").unwrap_or_else(|_| "news-scraper".to_string());

    let client = match kube::Client::try_default().await {
        Ok(c) => c,
        Err(e) => {
            error!("CronJob watcher: cannot connect to Kubernetes API: {} — scheduled runs will not notify", e);
            return;
        }
    };
    let job_api: Api<Job> = Api::namespaced(client.clone(), &namespace);

    info!(
        "CronJob watcher: watching Jobs owned by CronJob '{}' in namespace '{}'",
        cronjob_name, namespace
    );

    // Jobs already finished before this loop started. Seeded on the first pass
    // so a server restart does not replay notifications for old runs.
    let mut settled: HashSet<String> = HashSet::new();
    let mut announced_start: HashSet<String> = HashSet::new();
    let mut first_pass = true;

    loop {
        let jobs = match job_api.list(&ListParams::default()).await {
            Ok(list) => list.items,
            Err(e) => {
                warn!("CronJob watcher: cannot list Jobs: {}", e);
                time::sleep(time::Duration::from_secs(30)).await;
                continue;
            }
        };

        for job in jobs {
            let owned_by_cronjob = job
                .metadata
                .owner_references
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|o| o.kind == "CronJob" && o.name == cronjob_name);
            if !owned_by_cronjob {
                continue;
            }

            let Some(name) = job.metadata.name.clone() else { continue };
            if settled.contains(&name) {
                continue;
            }

            let status = job.status.unwrap_or_default();
            let succeeded = status.succeeded.unwrap_or(0);
            let failed = status.failed.unwrap_or(0);
            let active = status.active.unwrap_or(0);
            let is_terminal = succeeded > 0 || (failed > 0 && active == 0);

            // ── Running: mirror it into the UI, announce once ──────────
            if !is_terminal && active > 0 && announced_start.insert(name.clone()) {
                if first_pass {
                    // Already running when we started — adopt it silently
                    // rather than claiming it just began.
                    info!("CronJob watcher: adopting in-flight Job {}", name);
                } else {
                    info!("CronJob watcher: scheduled Job {} started", name);
                }
                if !state.is_scraping.swap(true, Ordering::SeqCst) {
                    let mut l = state.scraper_logs.lock().await;
                    l.clear();
                    l.push_back(format!("--- Scheduled run started (job {}) ---", name));
                    drop(l);
                    tokio::spawn(stream_pod_logs_for_job(
                        client.clone(),
                        namespace.clone(),
                        name.clone(),
                        Arc::clone(&state.scraper_logs),
                    ));
                }
                if !first_pass {
                    let cfg = load_app_config(&state.data_dir);
                    if cfg.ntfy_on_scheduled_run {
                        notify(&state.data_dir, NtfyMessage {
                            title: "News run started".to_string(),
                            body: format!(
                                "Scheduled daily run started at {} UTC (job: {}).",
                                Utc::now().format("%Y-%m-%d %H:%M"),
                                name
                            ),
                            priority: 2,
                            tags: "hourglass_flowing_sand",
                        })
                        .await;
                    }
                }
            }

            if !is_terminal {
                continue;
            }

            // ── Terminal: record it, then notify unless we're seeding ───
            settled.insert(name.clone());
            let success = succeeded > 0;

            if first_pass {
                // Pre-existing completed Job — remember it, stay quiet.
                continue;
            }

            info!(
                "CronJob watcher: scheduled Job {} finished ({})",
                name,
                if success { "succeeded" } else { "failed" }
            );

            state.last_run_success.store(success, Ordering::SeqCst);
            state.is_scraping.store(false, Ordering::SeqCst);

            // Give the log stream a moment to flush the tail of the run before
            // the failure excerpt snapshots it.
            time::sleep(time::Duration::from_millis(500)).await;

            // If we never saw this Job running we never streamed its logs, so
            // pull them now — the excerpt and the credential check both depend
            // on having the run's output.
            let have_output = state
                .scraper_logs
                .lock()
                .await
                .iter()
                .any(|l| !l.starts_with("---") && !l.trim().is_empty());
            if !have_output {
                fetch_job_logs_once(
                    client.clone(),
                    &namespace,
                    &name,
                    &Arc::clone(&state.scraper_logs),
                )
                .await;
            }

            let cfg = load_app_config(&state.data_dir);
            if !cfg.ntfy_enabled {
                continue;
            }
            let lines: Vec<String> = state.scraper_logs.lock().await.iter().cloned().collect();
            let msg = if success {
                cfg.ntfy_on_success.then(|| NtfyMessage {
                    title: "News run complete".to_string(),
                    body: format!("Scheduled daily run finished successfully (job: {}).", name),
                    priority: 2,
                    tags: "white_check_mark",
                })
            } else if looks_like_token_expiry(&lines) && cfg.ntfy_on_token_expiry {
                Some(NtfyMessage {
                    title: "LLM credentials rejected".to_string(),
                    body: format!(
                        "Scheduled daily run failed: the API key or OAuth token looks expired or invalid.\n\n{}",
                        failure_excerpt(&lines)
                    ),
                    priority: 5,
                    tags: "key,rotating_light",
                })
            } else {
                cfg.ntfy_on_failure.then(|| NtfyMessage {
                    title: "News run failed".to_string(),
                    body: format!(
                        "Scheduled daily run failed (job: {}).\n\n{}",
                        name,
                        failure_excerpt(&lines)
                    ),
                    priority: 4,
                    tags: "rotating_light",
                })
            };
            if let Some(msg) = msg {
                notify(&state.data_dir, msg).await;
            }
        }

        if first_pass {
            info!(
                "CronJob watcher: seeded {} already-finished Job(s) — notifications start from the next run",
                settled.len()
            );
        }

        // Drop bookkeeping for Jobs the TTL controller has deleted, so these
        // sets cannot grow without bound in a long-lived server. Re-seeding on
        // the next pass rather than clearing outright means the reset cannot
        // replay notifications for runs that finished long ago.
        if settled.len() > 200 {
            settled.clear();
            announced_start.clear();
            first_pass = true;
        } else {
            first_pass = false;
        }

        time::sleep(time::Duration::from_secs(30)).await;
    }
}

/// Dispatch to the Kubernetes Job runner or the local process fork,
/// depending on the `SCRAPE_RUNNER` environment variable.
fn trigger_scrape(
    state: AppState,
    envs: Vec<(&'static str, String)>,
    label: &'static str,
    trigger: RunTrigger,
) {
    if is_k8s_mode() {
        spawn_k8s_job(state, envs, label, trigger);
    } else {
        spawn_scraper_job(state, envs, label, trigger);
    }
}

/// Wakes every 30 s, checks the configured daily run time (UTC), and fires the
/// scraper when the clock matches — once per calendar day regardless of restarts.
///
/// In k8s mode this loop exits immediately: the CronJob owns the schedule,
/// keeping it declarative and in Git.  The server only handles UI/manual triggers.
async fn daily_scheduler_loop(state: AppState) {
    if is_k8s_mode() {
        info!("SCRAPE_RUNNER=k8s — internal daily scheduler disabled; CronJob owns the schedule");
        return;
    }
    let mut last_triggered_date: Option<String> = None;
    loop {
        time::sleep(time::Duration::from_secs(30)).await;
        let config = load_app_config(&state.data_dir);
        let now = Utc::now();
        let today = now.format("%Y-%m-%d").to_string();
        if now.hour() as u8 == config.daily_run_hour
            && now.minute() as u8 == config.daily_run_minute
            && last_triggered_date.as_deref() != Some(today.as_str())
        {
            last_triggered_date = Some(today.clone());
            if !state.is_scraping.swap(true, Ordering::SeqCst) {
                info!(
                    "Daily scheduler: triggering scraper at {:02}:{:02} UTC",
                    config.daily_run_hour, config.daily_run_minute
                );
                trigger_scrape(state.clone(), vec![], "scheduled daily run", RunTrigger::Scheduled);
            } else {
                info!("Daily scheduler: scraper already running at trigger time, skipping");
            }
        }
    }
}

/// Infinite loop that fires the cleanup task every 6 hours. Retention is
/// read from config each cycle so a Settings change applies without restart.
async fn cleanup_loop(data_dir: Arc<PathBuf>) {
    // Run once immediately on startup so stale files from a previous run are cleared.
    cleanup_old_files(&data_dir, load_app_config(&data_dir).cleanup_max_age_days as i64).await;

    let mut interval = time::interval(time::Duration::from_secs(6 * 60 * 60)); // 6 hours
    loop {
        interval.tick().await;
        cleanup_old_files(&data_dir, load_app_config(&data_dir).cleanup_max_age_days as i64).await;
    }
}

// ═══════════════════════════════════════════════════════════════════
// SERVER ENTRY POINT
// ═══════════════════════════════════════════════════════════════════

#[tokio::main]
async fn main() {
    // Initialise structured logging; level controllable via RUST_LOG env var.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("news_server=info".parse().unwrap())
                .add_directive("tower_http=warn".parse().unwrap()),
        )
        .init();

    // ── Resolve directories from environment variables ────────────
    let data_dir = Arc::new(PathBuf::from(
        std::env::var("DATA_DIR").unwrap_or_else(|_| "/app/data".to_string()),
    ));
    let frontend_dir = Arc::new(PathBuf::from(
        std::env::var("FRONTEND_DIR").unwrap_or_else(|_| "/app/frontend".to_string()),
    ));

    // Ensure both directories exist before serving
    for dir in [&*data_dir, &*frontend_dir] {
        std::fs::create_dir_all(dir)
            .unwrap_or_else(|e| panic!("Cannot create directory {:?}: {}", dir, e));
    }

    info!("Data directory    : {:?}", data_dir);
    info!("Frontend directory: {:?}", frontend_dir);
    info!(
        "Scrape runner     : {} (set SCRAPE_RUNNER=k8s to delegate to a Kubernetes Job)",
        std::env::var("SCRAPE_RUNNER").unwrap_or_else(|_| "local".to_string())
    );

    // Single source of truth for default sources: materialise config.json on
    // first boot. The scraper has no embedded defaults and reads this file.
    //
    // Disaster-recovery seed: if SEED_CONFIG (default /app/seed-config.json,
    // typically a git-managed ConfigMap mount) exists and parses as a valid
    // AppConfig, a fresh PVC starts from it instead of the built-in defaults.
    // An existing config.json always wins — UI edits are never overwritten.
    let config_path = data_dir.join("config.json");
    if !config_path.exists() {
        let seed_path = PathBuf::from(
            std::env::var("SEED_CONFIG").unwrap_or_else(|_| "/app/seed-config.json".to_string()),
        );
        let seed = std::fs::read_to_string(&seed_path).ok().filter(|s| {
            match serde_json::from_str::<AppConfig>(s) {
                Ok(_) => true,
                Err(e) => {
                    warn!("Seed config {:?} is invalid — ignoring it: {}", seed_path, e);
                    false
                }
            }
        });
        if let Some(seed_json) = seed {
            match std::fs::write(&config_path, &seed_json) {
                Ok(_) => info!("Seeded config.json from {:?}", seed_path),
                Err(e) => error!("Failed to write seeded config.json: {}", e),
            }
        } else {
            match serde_json::to_string_pretty(&AppConfig::default()) {
                Ok(json) => match std::fs::write(&config_path, json) {
                    Ok(_) => info!("Wrote default config.json → {:?}", config_path),
                    Err(e) => error!("Failed to write default config.json: {}", e),
                },
                Err(e) => error!("Failed to serialise default config: {}", e),
            }
        }
    }

    // ── Spawn background cleanup task ─────────────────────────────
    tokio::spawn(cleanup_loop(Arc::clone(&data_dir)));

    // ── Build the application router ──────────────────────────────
    let state = AppState {
        data_dir: Arc::clone(&data_dir),
        is_scraping: Arc::new(AtomicBool::new(false)),
        scraper_logs: Arc::new(tokio::sync::Mutex::new(std::collections::VecDeque::new())),
        last_run_success: Arc::new(AtomicBool::new(true)),
    };

    // ── Spawn internal daily scheduler ────────────────────────────
    // (exits immediately under SCRAPE_RUNNER=k8s — the CronJob owns the schedule)
    tokio::spawn(daily_scheduler_loop(state.clone()));

    // ── Watch CronJob-created Jobs so the scheduled run notifies ──
    // (exits immediately unless SCRAPE_RUNNER=k8s)
    tokio::spawn(cronjob_watcher_loop(state.clone()));

    let app = Router::new()
        // Version check for frontend upgrades
        .route("/api/version", get(handle_version))
        // JSON API for the frontend to discover media files
        .route("/api/media", get(handle_list_media))
        .route("/opds", get(handle_opds))
        .route("/api/config", get(handle_get_config).post(handle_post_config))
        .route("/api/sources/activity", get(handle_get_source_activity))
        .route("/api/scrape/status", get(handle_scrape_status))
        .route("/api/scrape/trigger", post(handle_scrape_trigger))
        .route("/api/scrape/regen-audio", post(handle_regen_audio))
        .route("/api/scrape/logs", get(handle_scrape_logs))
        .route("/api/tts/preview", get(handle_tts_preview))
        .route("/api/ntfy/test", post(handle_ntfy_test))
        // Serve generated media (EPUB + MP3) under /media/
        .nest_service("/media", ServeDir::new(&*data_dir))
        // Serve the single-page frontend for all other routes
        // (ServeDir with fallback to index.html enables client-side routing)
        .fallback_service(ServeDir::new(&*frontend_dir))
        // Permissive CORS — tighten in production if needed
        .layer(CorsLayer::permissive())
        // HTTP access logging via tracing
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    // ── Bind and serve ────────────────────────────────────────────
    let bind_addr = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:3000".to_string());

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .unwrap_or_else(|e| panic!("Cannot bind to {}: {}", bind_addr, e));

    info!("╔══════════════════════════════════════════╗");
    info!("║  News Server listening on http://{}  ║", bind_addr);
    info!("╚══════════════════════════════════════════╝");

    axum::serve(listener, app)
        .await
        .expect("Server error");
}

// ═══════════════════════════════════════════════════════════════════
// TESTS
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_escape_entities() {
        assert_eq!(
            xml_escape(r#"a & b <tag> "quoted""#),
            "a &amp; b &lt;tag&gt; &quot;quoted&quot;"
        );
        assert_eq!(xml_escape("plain"), "plain");
    }

    /// Temp dir that cleans itself up even if the test panics.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("news-test-{}-{}", name, std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn media_files_grouped_by_date_key_newest_first() {
        let tmp = TempDir::new("media");
        for f in [
            "daily-news-20260704-060101.epub",
            "daily-news-20260704-060101-x4.epub", // OPDS-only variant: never the dashboard epub
            "daily-tldr-20260704-060101.epub",
            "short-radio-20260704-060101.mp3",
            "long-podcast-20260704-060101.mp3",
            "daily-news-20260703-060131.epub",
            "notes.txt",           // ignored: not epub/mp3
            "random.epub",         // ignored: no embedded date
        ] {
            std::fs::write(tmp.0.join(f), b"").unwrap();
        }

        let entries = list_media_files(&tmp.0);
        assert_eq!(entries.len(), 2);

        // Newest date group first, with every artifact slotted correctly
        assert_eq!(entries[0].date, "20260704-060101");
        assert_eq!(entries[0].epub.as_deref(), Some("daily-news-20260704-060101.epub"));
        assert_eq!(entries[0].tldr.as_deref(), Some("daily-tldr-20260704-060101.epub"));
        assert_eq!(entries[0].radio.as_deref(), Some("short-radio-20260704-060101.mp3"));
        assert_eq!(entries[0].podcast.as_deref(), Some("long-podcast-20260704-060101.mp3"));

        assert_eq!(entries[1].date, "20260703-060131");
        assert_eq!(entries[1].tldr, None);
        assert_eq!(entries[1].radio, None);
    }

    #[test]
    fn app_config_defaults_are_permissive() {
        // A config.json written before these fields existed must deserialize
        // with everything enabled — this is what protects old installs.
        let cfg: AppConfig = serde_json::from_str(
            r#"{"rss_feeds": [], "medium_tags": []}"#
        ).unwrap();
        assert!(cfg.opds_enabled);
        assert!(cfg.enable_radio && cfg.enable_podcast && cfg.enable_tldr);
        assert!(cfg.skip_paywalled_posts);
        assert!(cfg.llm_radio.is_none());
    }

    /// The env overrides exist so the ntfy topic/token can come from a secret
    /// store instead of config.json on a PVC. These assertions pin the two
    /// behaviours that are easy to regress: a supplied topic switching
    /// notifications on, and an explicit NTFY_ENABLED still winning.
    ///
    /// Single test rather than several: env vars are process-global, so
    /// separate #[test] fns would race each other under the default harness.
    #[test]
    fn ntfy_env_overrides_config() {
        // Baseline: nothing set, config.json values survive untouched.
        for k in ["NTFY_SERVER", "NTFY_TOPIC", "NTFY_TOKEN", "NTFY_ENABLED"] {
            std::env::remove_var(k);
        }
        let mut cfg = AppConfig::default();
        cfg.ntfy_server = "https://from-config".to_string();
        cfg.ntfy_topic = "config-topic".to_string();
        apply_ntfy_env_overrides(&mut cfg);
        assert_eq!(cfg.ntfy_server, "https://from-config");
        assert_eq!(cfg.ntfy_topic, "config-topic");
        assert!(!cfg.ntfy_enabled, "default stays disabled without a topic");

        // A topic from the environment wins and turns notifications on —
        // otherwise ntfy_enabled's false default silently drops every message.
        std::env::set_var("NTFY_SERVER", "https://ntfy.internal");
        std::env::set_var("NTFY_TOPIC", "env-topic");
        std::env::set_var("NTFY_TOKEN", "tk_secret");
        let mut cfg = AppConfig::default();
        cfg.ntfy_server = "https://from-config".to_string();
        cfg.ntfy_topic = "config-topic".to_string();
        apply_ntfy_env_overrides(&mut cfg);
        assert_eq!(cfg.ntfy_server, "https://ntfy.internal");
        assert_eq!(cfg.ntfy_topic, "env-topic");
        assert_eq!(cfg.ntfy_token.as_deref(), Some("tk_secret"));
        assert!(cfg.ntfy_enabled);

        // Explicit disable beats the implicit enable, so an injected topic can
        // be muted without removing the secret from the deployment.
        std::env::set_var("NTFY_ENABLED", "false");
        let mut cfg = AppConfig::default();
        apply_ntfy_env_overrides(&mut cfg);
        assert!(!cfg.ntfy_enabled);

        // Blank values are ignored rather than blanking real config — an unset
        // ExternalSecret key renders as "".
        std::env::set_var("NTFY_TOPIC", "   ");
        std::env::remove_var("NTFY_ENABLED");
        let mut cfg = AppConfig::default();
        cfg.ntfy_topic = "config-topic".to_string();
        apply_ntfy_env_overrides(&mut cfg);
        assert_eq!(cfg.ntfy_topic, "config-topic");
        assert!(!cfg.ntfy_enabled);

        for k in ["NTFY_SERVER", "NTFY_TOPIC", "NTFY_TOKEN", "NTFY_ENABLED"] {
            std::env::remove_var(k);
        }
    }
}
