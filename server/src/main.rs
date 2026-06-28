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

#[derive(Serialize)]
struct ScrapeStatus {
    running: bool,
}

/// `GET /api/scrape/status` — returns whether the scraper is currently running.
async fn handle_scrape_status(
    State(state): State<AppState>,
) -> Json<ScrapeStatus> {
    Json(ScrapeStatus {
        running: state.is_scraping.load(Ordering::Relaxed),
    })
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

    let is_scraping = Arc::clone(&state.is_scraping);
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
        
        match cmd.spawn() {
            Ok(mut child) => {
                match child.wait().await {
                    Ok(status) => {
                        info!("Background scraper completed with status: {:?}", status);
                    }
                    Err(e) => {
                        error!("Failed to wait for background scraper process: {}", e);
                    }
                }
            }
            Err(e) => {
                error!("Failed to spawn background scraper process ({} {}): {}", python_bin, scraper_script, e);
            }
        }
        
        is_scraping.store(false, Ordering::SeqCst);
    });

    Ok(Json(ScrapeStatus { running: true }))
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
    };

    let app = Router::new()
        // JSON API for the frontend to discover media files
        .route("/api/media", get(handle_list_media))
        .route("/api/scrape/status", get(handle_scrape_status))
        .route("/api/scrape/trigger", post(handle_scrape_trigger))
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
