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
    extract::State,
    http::StatusCode,
    response::Json,
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
struct AppConfig {
    rss_feeds: Vec<RssFeed>,
    medium_tags: Vec<String>,
    #[serde(default)]
    silenced_sources: Vec<String>,
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
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(config) = serde_json::from_str::<AppConfig>(&content) {
                return Json(config);
            }
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

use axum::extract::Query;

#[derive(Deserialize)]
struct TriggerParams {
    voice_short: Option<String>,
    voice_long: Option<String>,
    short_sources: Option<String>,
    long_sources: Option<String>,
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

    let python_bin = std::env::var("PYTHON_BIN").unwrap_or_else(|_| "python3".to_string());
    let scraper_script = std::env::var("SCRAPER_SCRIPT").unwrap_or_else(|_| "scraper/scraper.py".to_string());

    let state_clone = state.clone();
    tokio::spawn(async move {
        info!("Spawning background scraper process: {} {}", python_bin, scraper_script);
        
        let mut cmd = tokio::process::Command::new(&python_bin);
        cmd.arg(&scraper_script);
        
        // Pass query parameters to child process environment
        if let Some(vs) = params.voice_short {
            cmd.env("VOICE_SHORT", vs);
        }
        if let Some(vl) = params.voice_long {
            cmd.env("VOICE_LONG", vl);
        }
        if let Some(ss) = params.short_sources {
            cmd.env("SHORT_SOURCES", ss);
        }
        if let Some(ls) = params.long_sources {
            cmd.env("LONG_SOURCES", ls);
        }

        // Pipe stdout & stderr to capture logs in real time
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        // Clear logs at start of a new run
        {
            let mut logs = state_clone.scraper_logs.lock().await;
            logs.clear();
            logs.push_back("--- Starting news scraper pipeline ---".to_string());
        }
        state_clone.last_run_success.store(true, Ordering::SeqCst);
        
        match cmd.spawn() {
            Ok(mut child) => {
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                
                const MAX_LOG_LINES: usize = 150;
                
                // Spawn task to read stdout
                let logs_stdout = Arc::clone(&state_clone.scraper_logs);
                if let Some(out) = stdout {
                    tokio::spawn(async move {
                        use tokio::io::{AsyncBufReadExt, BufReader};
                        let mut reader = BufReader::new(out).lines();
                        while let Ok(Some(line)) = reader.next_line().await {
                            let mut l = logs_stdout.lock().await;
                            l.push_back(line);
                            if l.len() > MAX_LOG_LINES {
                                l.pop_front();
                            }
                        }
                    });
                }
                
                // Spawn task to read stderr
                let logs_stderr = Arc::clone(&state_clone.scraper_logs);
                if let Some(err) = stderr {
                    tokio::spawn(async move {
                        use tokio::io::{AsyncBufReadExt, BufReader};
                        let mut reader = BufReader::new(err).lines();
                        while let Ok(Some(line)) = reader.next_line().await {
                            let mut l = logs_stderr.lock().await;
                            l.push_back(format!("[ERROR] {}", line));
                            if l.len() > MAX_LOG_LINES {
                                l.pop_front();
                            }
                        }
                    });
                }

                match child.wait().await {
                    Ok(status) => {
                        info!("Background scraper completed with status: {:?}", status);
                        let success = status.success();
                        state_clone.last_run_success.store(success, Ordering::SeqCst);
                        let mut l = state_clone.scraper_logs.lock().await;
                        if success {
                            l.push_back("--- Pipeline completed successfully ---".to_string());
                        } else {
                            l.push_back(format!("--- Pipeline failed with exit status: {:?} ---", status));
                        }
                    }
                    Err(e) => {
                        error!("Failed to wait for background scraper process: {}", e);
                        state_clone.last_run_success.store(false, Ordering::SeqCst);
                        let mut l = state_clone.scraper_logs.lock().await;
                        l.push_back(format!("--- Error waiting for pipeline: {} ---", e));
                    }
                }
            }
            Err(e) => {
                error!("Failed to spawn background scraper process ({} {}): {}", python_bin, scraper_script, e);
                state_clone.last_run_success.store(false, Ordering::SeqCst);
                let mut l = state_clone.scraper_logs.lock().await;
                l.push_back(format!("--- Failed to spawn scraper process: {} ---", e));
            }
        }
        
        state_clone.is_scraping.store(false, Ordering::SeqCst);
    });

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

    let python_bin = std::env::var("PYTHON_BIN").unwrap_or_else(|_| "python3".to_string());
    let scraper_script = std::env::var("SCRAPER_SCRIPT").unwrap_or_else(|_| "scraper/scraper.py".to_string());

    let state_clone = state.clone();
    tokio::spawn(async move {
        info!("Spawning audio regen for date: {}", date_str);

        let mut cmd = tokio::process::Command::new(&python_bin);
        cmd.arg(&scraper_script)
           .env("REGEN_DATE", &date_str);

        if let Some(vs) = params.voice_short { cmd.env("VOICE_SHORT", vs); }
        if let Some(vl) = params.voice_long  { cmd.env("VOICE_LONG", vl); }
        if let Some(ss) = params.short_sources { cmd.env("SHORT_SOURCES", ss); }
        if let Some(ls) = params.long_sources  { cmd.env("LONG_SOURCES", ls); }

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        {
            let mut logs = state_clone.scraper_logs.lock().await;
            logs.clear();
            logs.push_back(format!("--- Starting audio regen for {} ---", date_str));
        }
        state_clone.last_run_success.store(true, Ordering::SeqCst);

        match cmd.spawn() {
            Ok(mut child) => {
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();

                const MAX_LOG_LINES: usize = 150;

                let logs_stdout = Arc::clone(&state_clone.scraper_logs);
                if let Some(out) = stdout {
                    tokio::spawn(async move {
                        use tokio::io::{AsyncBufReadExt, BufReader};
                        let mut reader = BufReader::new(out).lines();
                        while let Ok(Some(line)) = reader.next_line().await {
                            let mut l = logs_stdout.lock().await;
                            l.push_back(line);
                            if l.len() > MAX_LOG_LINES { l.pop_front(); }
                        }
                    });
                }

                let logs_stderr = Arc::clone(&state_clone.scraper_logs);
                if let Some(err) = stderr {
                    tokio::spawn(async move {
                        use tokio::io::{AsyncBufReadExt, BufReader};
                        let mut reader = BufReader::new(err).lines();
                        while let Ok(Some(line)) = reader.next_line().await {
                            let mut l = logs_stderr.lock().await;
                            l.push_back(format!("[ERROR] {}", line));
                            if l.len() > MAX_LOG_LINES { l.pop_front(); }
                        }
                    });
                }

                match child.wait().await {
                    Ok(status) => {
                        let success = status.success();
                        state_clone.last_run_success.store(success, Ordering::SeqCst);
                        let mut l = state_clone.scraper_logs.lock().await;
                        if success {
                            l.push_back("--- Audio regen completed successfully ---".to_string());
                        } else {
                            l.push_back(format!("--- Audio regen failed with exit status: {:?} ---", status));
                        }
                    }
                    Err(e) => {
                        state_clone.last_run_success.store(false, Ordering::SeqCst);
                        let mut l = state_clone.scraper_logs.lock().await;
                        l.push_back(format!("--- Error during audio regen: {} ---", e));
                    }
                }
            }
            Err(e) => {
                state_clone.last_run_success.store(false, Ordering::SeqCst);
                let mut l = state_clone.scraper_logs.lock().await;
                l.push_back(format!("--- Failed to spawn regen process: {} ---", e));
            }
        }

        state_clone.is_scraping.store(false, Ordering::SeqCst);
    });

    Ok(Json(ScrapeStatus {
        running: true,
        last_run_success: state.last_run_success.load(Ordering::SeqCst),
    }))
}

use axum::response::IntoResponse;
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
        // JSON API for the frontend to discover media files
        .route("/api/media", get(handle_list_media))
        .route("/api/config", get(handle_get_config).post(handle_post_config))
        .route("/api/sources/activity", get(handle_get_source_activity))
        .route("/api/scrape/status", get(handle_scrape_status))
        .route("/api/scrape/trigger", post(handle_scrape_trigger))
        .route("/api/scrape/regen-audio", post(handle_regen_audio))
        .route("/api/scrape/logs", get(handle_scrape_logs))
        .route("/api/tts/preview", get(handle_tts_preview))
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
