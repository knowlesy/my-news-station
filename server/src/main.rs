//! ╔══════════════════════════════════════════════════════════════════╗
//! ║          Daily News Media Station — Axum Web Server             ║
//! ║                                                                  ║
//! ║  Routes:                                                         ║
//! ║    GET  /            → serves ./frontend/ (static SPA)           ║
//! ║    GET  /media/*     → serves ./data/     (EPUB + MP3 files)     ║
//! ║    GET  /api/media   → JSON list of available dated media files  ║
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
use chrono::{DateTime, Duration, Utc};
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
struct CrosspointDevice {
    name: String,
    ip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppConfig {
    rss_feeds: Vec<RssFeed>,
    medium_tags: Vec<String>,
    #[serde(default)]
    silenced_sources: Vec<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    crosspoint_devices: Vec<CrosspointDevice>,
    #[serde(default)]
    default_crosspoint_ip: Option<String>,
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
}

fn default_skip_paywalled() -> bool {
    true
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
            system_prompt: None,
            crosspoint_devices: Vec::new(),
            default_crosspoint_ip: None,
            voice_short: None,
            voice_long: None,
            sources_short: Vec::new(),
            sources_long: Vec::new(),
            skip_paywalled_posts: true,
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
            radio:   None,
            podcast: None,
        });

        if file_name.starts_with("daily-news-") && file_name.ends_with(".epub") {
            media.epub = Some(file_name);
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
}

/// `GET /api/version` — return the current release version and build date.
async fn handle_version() -> Json<VersionResponse> {
    let date = Utc::now().format("%Y-%m-%d").to_string();
    Json(VersionResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        date,
    })
}

/// `GET /api/media` — return a JSON list of all available media grouped by date.
async fn handle_list_media(
    State(state): State<AppState>,
) -> Result<Json<MediaListResponse>, StatusCode> {
    let dates = list_media_files(&state.data_dir);
    Ok(Json(MediaListResponse { dates }))
}

/// `GET /api/config` — read the current sources configuration from config.json or return defaults.
async fn handle_get_config(
    State(state): State<AppState>,
) -> Json<AppConfig> {
    let path = state.data_dir.join("config.json");
    if path.exists() {
        match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<AppConfig>(&content) {
                Ok(config) => return Json(config),
                Err(e) => warn!(
                    "config.json is corrupt — serving defaults (file NOT overwritten; \
                     a Save from the UI will replace it): {}",
                    e
                ),
            },
            Err(e) => warn!("Cannot read config.json — serving defaults: {}", e),
        }
    }
    Json(AppConfig::default())
}

/// `POST /api/config` — save the new sources configuration to config.json.
async fn handle_post_config(
    State(state): State<AppState>,
    Json(payload): Json<AppConfig>,
) -> Result<StatusCode, StatusCode> {
    let path = state.data_dir.join("config.json");
    if let Ok(content) = serde_json::to_string_pretty(&payload) {
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
/// The caller must have already claimed the `is_scraping` flag; this function
/// releases it when the child exits. `label` names the job in log lines
/// ("Pipeline", "Audio regen").
fn spawn_scraper_job(state: AppState, envs: Vec<(&'static str, String)>, label: &'static str) {
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

    spawn_scraper_job(state.clone(), envs, "news scraper pipeline");

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
    if let Some(vs) = params.voice_short { envs.push(("VOICE_SHORT", vs)); }
    if let Some(vl) = params.voice_long { envs.push(("VOICE_LONG", vl)); }
    if let Some(ss) = params.short_sources { envs.push(("SHORT_SOURCES", ss)); }
    if let Some(ls) = params.long_sources { envs.push(("LONG_SOURCES", ls)); }

    spawn_scraper_job(state.clone(), envs, "audio regen");

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
// CROSSPOINT DEVICE API
// ═══════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
struct ProbeParams {
    ip: String,
}

#[derive(Debug, Serialize)]
struct ProbeResponse {
    status: String,   // "online_crosspoint" | "online_stock" | "offline"
    firmware: Option<String>,
}

/// `GET /api/crosspoint/probe?ip=X` — probe whether an X4 device is reachable.
/// Tries CrossPoint firmware first (/api/status), then stock (/list?dir=/).
async fn handle_crosspoint_probe(
    axum::extract::Query(params): axum::extract::Query<ProbeParams>,
) -> Json<ProbeResponse> {
    // Sanitise: only allow IPs / hostnames with safe chars
    let ip = &params.ip;
    if !ip.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_') {
        return Json(ProbeResponse { status: "offline".into(), firmware: None });
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap_or_default();

    // CrossPoint firmware
    if let Ok(resp) = client.get(format!("http://{}/api/status", ip)).send().await {
        if resp.status().is_success() {
            return Json(ProbeResponse {
                status: "online_crosspoint".into(),
                firmware: Some("crosspoint".into()),
            });
        }
    }

    // Stock firmware
    if let Ok(resp) = client.get(format!("http://{}/list?dir=/", ip)).send().await {
        if resp.status().is_success() {
            return Json(ProbeResponse {
                status: "online_stock".into(),
                firmware: Some("stock".into()),
            });
        }
    }

    Json(ProbeResponse { status: "offline".into(), firmware: None })
}

#[derive(Debug, Deserialize)]
struct SendPayload {
    ip: String,
    firmware: String,  // "crosspoint" | "stock"
    date: String,      // YYYYMMDD
}

#[derive(Debug, Serialize)]
struct SendResponse {
    success: bool,
    message: String,
    already_sent: bool,
}

/// `POST /api/crosspoint/send` — read the EPUB for `date` and push it to the X4.
async fn handle_crosspoint_send(
    State(state): State<AppState>,
    Json(payload): Json<SendPayload>,
) -> Json<SendResponse> {
    let ip = &payload.ip;
    if !ip.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_') {
        return Json(SendResponse { success: false, message: "Invalid device IP".into(), already_sent: false });
    }

    // Locate the EPUB for the requested date. The scraper writes
    // daily-news-{YYYYMMDD-HHMMSS}.epub and the playlist group key is that
    // embedded date, so a prefix scan finds it; pick the newest on a tie.
    if payload.date.is_empty() || !payload.date.chars().all(|c| c.is_ascii_digit() || c == '-') {
        return Json(SendResponse { success: false, message: "Invalid date".into(), already_sent: false });
    }
    let prefix = format!("daily-news-{}", payload.date);
    let epub_path = std::fs::read_dir(&*state.data_dir)
        .ok()
        .and_then(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with(&prefix) && n.ends_with(".epub"))
                        .unwrap_or(false)
                })
                .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())
        });
    let Some(epub_path) = epub_path else {
        return Json(SendResponse {
            success: false,
            message: format!("EPUB not found for date {}", payload.date),
            already_sent: false,
        });
    };

    // Check sent history
    let history_path = state.data_dir.join("crosspoint_sent.json");
    let mut history: serde_json::Value = if history_path.exists() {
        std::fs::read_to_string(&history_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let history_key = format!("{}_{}", payload.date, ip);
    if history.get(&history_key).is_some() {
        return Json(SendResponse {
            success: true,
            message: "Already sent — file is on the device.".into(),
            already_sent: true,
        });
    }

    let epub_bytes = match std::fs::read(&epub_path) {
        Ok(b) => b,
        Err(e) => return Json(SendResponse {
            success: false,
            message: format!("Failed to read EPUB: {}", e),
            already_sent: false,
        }),
    };

    let filename = epub_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| format!("daily-news-{}.epub", payload.date));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();

    let part = reqwest::multipart::Part::bytes(epub_bytes)
        .file_name(filename.clone())
        .mime_str("application/epub+zip")
        .unwrap();

    let upload_result = if payload.firmware == "crosspoint" {
        let form = reqwest::multipart::Form::new().part("file", part);
        client
            .post(format!("http://{}/upload?path=/", ip))
            .multipart(form)
            .send()
            .await
    } else {
        // Stock firmware
        let form = reqwest::multipart::Form::new()
            .part("name", reqwest::multipart::Part::text(format!("/{}", filename)))
            .part("data", part);
        client
            .post(format!("http://{}/edit", ip))
            .multipart(form)
            .send()
            .await
    };

    match upload_result {
        Ok(resp) if resp.status().is_success() || resp.status().as_u16() == 302 => {
            // Record in sent history
            history[&history_key] = serde_json::json!(chrono::Utc::now().to_rfc3339());
            let _ = std::fs::write(&history_path, serde_json::to_string_pretty(&history).unwrap_or_default());
            info!("Crosspoint: sent {} to {}", filename, ip);
            Json(SendResponse { success: true, message: "File sent successfully.".into(), already_sent: false })
        }
        Ok(resp) => {
            let status = resp.status().as_u16();
            warn!("Crosspoint: device {} returned HTTP {}", ip, status);
            Json(SendResponse {
                success: false,
                message: format!("Device returned HTTP {}", status),
                already_sent: false,
            })
        }
        Err(e) => {
            warn!("Crosspoint: send to {} failed: {}", ip, e);
            Json(SendResponse { success: false, message: format!("Connection failed: {}", e), already_sent: false })
        }
    }
}

/// `GET /api/crosspoint/history` — return sent history so the UI can check per-date state.
async fn handle_crosspoint_history(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let path = state.data_dir.join("crosspoint_sent.json");
    if path.exists() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                return Ok(Json(v));
            }
        }
    }
    Ok(Json(serde_json::json!({})))
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

/// Infinite loop that fires the cleanup task every 6 hours.
async fn cleanup_loop(data_dir: Arc<PathBuf>) {
    // Run once immediately on startup so stale files from a previous run are cleared.
    cleanup_old_files(&data_dir, 10).await;

    let mut interval = time::interval(time::Duration::from_secs(6 * 60 * 60)); // 6 hours
    loop {
        interval.tick().await;
        cleanup_old_files(&data_dir, 10).await;
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

    // Single source of truth for default sources: materialise config.json on
    // first boot. The scraper has no embedded defaults and reads this file.
    let config_path = data_dir.join("config.json");
    if !config_path.exists() {
        match serde_json::to_string_pretty(&AppConfig::default()) {
            Ok(json) => match std::fs::write(&config_path, json) {
                Ok(_) => info!("Wrote default config.json → {:?}", config_path),
                Err(e) => error!("Failed to write default config.json: {}", e),
            },
            Err(e) => error!("Failed to serialise default config: {}", e),
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

    let app = Router::new()
        // Version check for frontend upgrades
        .route("/api/version", get(handle_version))
        // JSON API for the frontend to discover media files
        .route("/api/media", get(handle_list_media))
        .route("/api/config", get(handle_get_config).post(handle_post_config))
        .route("/api/sources/activity", get(handle_get_source_activity))
        .route("/api/scrape/status", get(handle_scrape_status))
        .route("/api/scrape/trigger", post(handle_scrape_trigger))
        .route("/api/scrape/regen-audio", post(handle_regen_audio))
        .route("/api/scrape/logs", get(handle_scrape_logs))
        .route("/api/tts/preview", get(handle_tts_preview))
        .route("/api/crosspoint/probe", get(handle_crosspoint_probe))
        .route("/api/crosspoint/send", post(handle_crosspoint_send))
        .route("/api/crosspoint/history", get(handle_crosspoint_history))
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
