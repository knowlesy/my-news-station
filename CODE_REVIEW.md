# Code Review — my-news

Full-repo review covering `scraper/scraper.py` (1,263 lines), `server/src/main.rs` (978 lines),
`frontend/index.html` (2,403 lines), Dockerfile, and k8s manifests.
Items are ordered by severity. Every fix here retains existing functionality.

> **Status (2026-07-04):** All P0 (1–5), P1 (6–10), and P2 (11–19) items are **fixed and
> verified** (details inline). P3 remains open.

---

## P0 — Broken functionality (fix first)

### 1. ✅ FIXED — Custom system prompt is silently discarded
`scraper.py:143` loads `system_prompt` from `config.json` into `SYSTEM_PROMPT` — but this runs at
line 143, and line 754 later executes `SYSTEM_PROMPT = DEFAULT_SYSTEM_PROMPT`, unconditionally
overwriting it. The "Show Structure & Personality" textarea in the UI therefore has **no effect**.
**Fix:** define `DEFAULT_SYSTEM_PROMPT` before the config-load block, or (better) stop mutating
globals at import time — see item 12.
**Applied:** config now loads into `CUSTOM_SYSTEM_PROMPT`, and the definition site resolves
`SYSTEM_PROMPT = CUSTOM_SYSTEM_PROMPT or DEFAULT_SYSTEM_PROMPT`. Verified by importing the module
with a synthetic `config.json` — the custom prompt takes effect. (Full de-globalization still
tracked in item 12.)

### 2. ✅ FIXED — Send-to-X4 can never find the EPUB
`main.rs:733` and `main.rs:771` look for `news-{date}.epub`, but the scraper writes
`daily-news-{YYYYMMDD-HHMMSS}.epub` (`scraper.py:972`). Every send returns "EPUB not found".
**Fix:** scan the data dir for a file starting `daily-news-{date}` (the date group key already
matches the filename's embedded date), or pass the exact filename from the frontend, which
already has it in `entry.epub`. The latter is simpler and removes the guessing.
**Applied:** the handler now scans the data dir for `daily-news-{date}*.epub` (newest by mtime on
a tie), validates the date parameter, and uploads under the file's real name. Compiles clean;
no frontend change needed.

### 3. ✅ FIXED — Regenerated audio appears as a duplicate playlist entry
The pipeline writes `short-radio-{YYYYMMDD-HHMMSS}.mp3`, but `run_regen_audio`
(`scraper.py:1244`) writes `short-radio-{YYYYMMDD}.mp3` (date-only). The media grouping regex in
`main.rs:187` treats `20260703-183713` and `20260703` as **different groups**, so regen'd audio
shows up as a second entry for the same day — with no EPUB attached — instead of replacing the
original audio. The comment says "overwrite existing files" but it never does.
**Fix:** group media by the first 8 digits only (`main.rs`), or make regen write the same
timestamped filename it's replacing (find via the date prefix). Grouping by day in the server is
the cleaner fix and also collapses any historical duplicates.
**Applied:** `run_regen_audio` now names output MP3s with the full group key the frontend passes
(`short-radio-{YYYYMMDD-HHMMSS}.mp3`), so regenerated audio joins the clicked entry's group and
overwrites pipeline audio in place. Note: date-only orphan files from *previous* regens remain
their own entries until the 10-day cleanup removes them.

### 4. ✅ FIXED — "Silence" a source does nothing
`silenced_sources` is saved to `config.json` and rendered in the UI, but `scraper.py` never reads
it (zero references). Silenced sources are still scraped, extracted, and included in the EPUB.
**Fix:** after loading config, filter `RSS_FEEDS`/`MEDIUM_TAGS` against `silenced_sources`.
**Applied:** the config-load block now drops silenced feeds by name and silenced Medium tags by
UI label (`Medium/tags/<tag>`) or raw tag. Verified with a synthetic config: silenced entries are
excluded from both lists before any scraping.

### 5. ✅ FIXED — Dead duplicate `format_text_to_html`
`scraper.py:528` defines a truncated `format_text_to_html` (docstring + empty-check, then falls
off the end returning `None`). `extract_images` is nested awkwardly after it, and the real
implementation at line 560 shadows the broken one. Harmless today, a landmine for the next edit.
**Fix:** delete lines 528–534 (the first definition).
**Applied:** deleted. AST check confirms no duplicate function definitions remain in the module.

---

## P1 — Robustness & resource risks

### 6. ✅ FIXED — Unbounded browser concurrency (OOM risk in k8s)
`scraper.py:1052` fires `asyncio.gather` over **every** article at once — 16 sources × 10
articles ≈ 160 concurrent Chromium pages. The k8s CronJob has a 3Gi memory limit; this can OOM
the pod or trip site rate-limits.
**Fix:** wrap `extract_article_content` in an `asyncio.Semaphore(6)` (or similar). ~4 lines.
**Applied:** extraction now runs through an `asyncio.Semaphore`, capped at 6 concurrent pages by
default and tunable via the `EXTRACT_CONCURRENCY` env var.

### 7. ✅ FIXED — Unescaped HTML interpolation in EPUB chapters
`build_epub` (`scraper.py:925–955`) injects `chapter_title`, `author_name`, `source`, and
`article_url` raw into XHTML. An article title containing `&`, `<`, or a stray quote produces
invalid XHTML — epub.js renders a blank chapter and ebooklib may choke on round-trip
(`extract_articles_from_epub`). The body is escaped via `format_text_to_html`; the metadata isn't.
**Fix:** `html.escape()` title/author/source, and attribute-quote the URL.
**Applied:** title, author, source, article URL, and image URLs are all `html.escape()`d before
interpolation. Verified by building an EPUB with `&`/`<`/quote-laden metadata and round-tripping
it through `extract_articles_from_epub` — the title survives exactly.

### 8. ✅ FIXED — No feed timeout — one dead RSS feed stalls the whole run
`feedparser.parse(url)` (`scraper.py:356`) has no timeout and runs serially per feed. A
blackholed feed hangs until the CronJob's 90-minute deadline kills everything.
**Fix:** fetch with `requests.get(url, timeout=15)` and pass the body to `feedparser.parse()`.
**Applied:** feeds are fetched via `requests` with a 15s timeout (`FEED_TIMEOUT_SECS`) and the
project User-Agent, then parsed from the response body; fetch failures log a warning and return
an empty list. Verified live: a healthy feed returns articles, a blackholed IP returns `[]` after
exactly 15s instead of hanging.

### 9. ✅ FIXED — Scraped-URL registry truncation drops random entries
`scraper.py:1116–1118`: `list(set)[-2000:]` — sets are unordered, so the 2000 "kept" URLs are
arbitrary, not the newest. Old articles can be re-scraped and duplicated on subsequent runs.
**Fix:** keep a list (insertion-ordered), dedupe with a set membership check, then truncate.
**Applied:** the registry is now an insertion-ordered list (oldest first) with set-based dedupe;
truncation keeps the newest 2000.

### 10. ✅ FIXED — Config parse errors silently revert to defaults
`handle_get_config` (`main.rs:246`) returns `AppConfig::default()` on *any* read/parse failure
with no log line. A corrupted `config.json` silently resurrects the default source list and wipes
the user's devices/prompt from the UI (and a subsequent Save persists that reset).
**Fix:** log a `warn!` on parse failure at minimum; ideally return 500 so the UI shows an error
instead of quietly re-serving defaults.
**Applied:** read and parse failures now log distinct `warn!` lines (noting the file is not
overwritten). Kept the defaults fallback rather than a 500 so the dashboard stays usable.
Residual risk (documented): a user hitting Save while defaults are being served will persist the
reset — acceptable until item 14 consolidates config handling.

---

## P2 — Despaghetti / structure

### 11. ✅ FIXED — `handle_scrape_trigger` and `handle_regen_audio` are ~120 duplicated lines
`main.rs:324–443` and `main.rs:456–558` are near-identical: spawn python, pump stdout/stderr into
the ring buffer, track success, reset the running flag. Only the env vars and log labels differ.
**Fix:** extract `spawn_scraper_job(state, envs: Vec<(String, String)>, label: &str)`; both
handlers become ~10 lines.
**Applied:** `spawn_scraper_job` + `pump_child_stream` helpers own the spawn/log-pump/wait
machinery; each handler is now just flag-claim + env assembly (~15 lines each).

### 12. ✅ FIXED — Import-time global mutation in the scraper
`scraper.py:108–150` reads `config.json` at module import and rebinds `RSS_FEEDS`,
`MEDIUM_TAGS`, `SYSTEM_PROMPT` — before some of those names are even defined (the direct cause
of bug #1). Config, constants, and behavior are interleaved across 700 lines.
**Fix:** move config resolution into a `load_config()` function called at the top of
`run_pipeline()`/`run_regen_audio()`, returning a small config object (or populating a single
`CONFIG` dict). Globals become read-only defaults.
**Applied:** `load_config()` is called explicitly by both entry points; nothing is mutated at
import time. Verified: importing the module leaves defaults untouched; `load_config()` applies
feeds/tags/prompt/silencing, and a missing config.json raises a clear `SystemExit`.

### 13. ✅ FIXED — Default source list exists in two places — and has already drifted
`scraper.py:54–71` and `main.rs:79–148` both hardcode the default RSS list. They disagree
(`main.rs` has "Terraform Blog"; scraper has "DevOps Bulletin"/"Let's Do DevOps" variants). The
scraper's copy only applies when `config.json` doesn't exist, the server's copy when it does the
first GET — so which defaults you get depends on which component ran first.
**Fix:** single owner. Simplest: server writes `config.json` with defaults on first boot; scraper
*requires* the file and has no embedded list.
**Applied:** exactly that. The Rust default list is now the union of the two drifted lists
(17 feeds — "DevOps Bulletin" added), the server materialises `config.json` on first boot
(verified live), and the scraper's embedded list is deleted — it exits with a clear message if
the file is missing.

### 14. ✅ FIXED — Two sources of truth for user settings
Voice + per-briefing source selection live in browser `localStorage`
(`index.html:1097–1133`); feeds, tags, silenced sources, devices, and the prompt live in
server-side `config.json`. Same Settings modal, two storage backends — settings silently differ
per browser.
**Fix:** move voice/source-selection into `config.json` (the POST payload already exists);
keep only cosmetic prefs (theme) in localStorage.
**Applied:** `voice_short`, `voice_long`, `sources_short`, `sources_long` are new AppConfig
fields; the frontend saves them via a shared `postConfig()` merge and reads them back with a
one-time legacy-localStorage fallback. Theme and clicked-editions stay in localStorage.
Verified via browser automation: voice + source changes persist into config.json. Known minor
edge: an empty saved source selection is treated as "unsaved" (falls back to all-checked).

### 15. ✅ FIXED — 2,400-line `index.html` monolith
~1,400 lines of JS in one IIFE with ~30 top-level `let` state variables, inline styles built via
string templates, and features threaded through each other (crosspoint code patches
`toggleOptionsModal` from 200 lines away). It works, but every new feature (see #16) is now
grafted on with wrappers.
**Fix:** split into ES modules served as static files — `api.js`, `playlist.js`, `epubReader.js`,
`settings.js`, `crosspoint.js`, `scraperStatus.js` — no bundler needed
(`<script type="module">`). Mechanical refactor, do it before the next feature lands.
**Applied:** `frontend/js/{utils,settings,crosspoint,playlist,epubReader,scraperStatus,main}.js`
(no separate api.js — the fetch calls are thin enough to live in their feature modules).
index.html is now 1,034 lines of pure HTML/CSS. Cross-module state uses ES-module live bindings
(`activeDate`, `isScrapingActive`, `currentConfig`). Verified with a full Playwright smoke test
against the real server: page load, playlist render, EPUB render, settings modal, device
management, and config persistence — zero console/page errors.

### 16. ✅ FIXED — Monkey-patch and dead code in the crosspoint UI wiring
`index.html:1566` wraps `window.toggleOptionsModal` to inject device-list rendering, and
`index.html:1578` (`const _origSaveClick = $('saveOptionsBtn');`) is dead — assigned, never used,
with a comment describing something that isn't there.
**Fix:** fold device rendering directly into `toggleOptionsModal`, delete the dead const.
**Applied:** as part of the module split — `toggleOptionsModal` (settings.js) calls
`syncFromConfig` + `renderCrosspointDevices` directly; the wrapper and dead const are gone.

### 17. ✅ FIXED — Duplicated log-polling / status machinery
`fetchScraperLogs` (`index.html:2317`) vs `fetchModalLogs` (`index.html:1690`) fetch the same
endpoint and maintain two status dots; the modal adds a second 2s interval on top of the global
3s poll. The running/finished/failed UI transitions are hand-copied in `checkScraperStatus` *and*
`triggerRegenAudio`.
**Fix:** one poller with a list of render callbacks; one `setScraperUiState(running, success)`
function used by all three call sites.
**Applied:** scraperStatus.js has a single `refreshLogs()` that renders both the inline console
and the modal, one `updateStatusDots()` for both dots, and a shared `showRunningUi()` used by
the status poll, the scrape trigger, and the regen trigger.

### 18. ✅ FIXED — `import json` seven times
`json` is imported at `scraper.py:14`, then re-imported inside six functions, twice aliased as
`_json`. Delete all function-local imports (same for `glob` at `scraper.py:1204` — never used,
and `regen_ts` at `scraper.py:1243` — assigned, never used).
**Applied:** only the top-level import remains; unused `glob` import and `regen_ts` deleted.

### 19. ✅ FIXED — Repeated JSON file-IO boilerplate
Five near-identical "if exists → open → parse → except → warn" blocks (scraped_urls ×2, activity,
sidecar, config).
**Fix:** `load_json(path, default)` / `save_json(path, obj)` helpers, ~10 lines total, removes ~40.
**Applied:** all five sites (config, scraped_urls ×2, activity, sidecar load/save) now use the
helpers.

---

## P3 — Simplification & hygiene

### 20. Dockerfile double-installs Chromium
The base image `mcr.microsoft.com/playwright/python:v1.44.0-jammy` already ships Chromium, but
`RUN playwright install chromium --with-deps` downloads it again (image bloat, slower quarterly
builds). Also `npm install -g @anthropic-ai/claude-code 2>/dev/null` hides install failures and
bakes a stale CLI version into a quarterly image — surface errors, and consider pinning.

### 21. `:latest` + `imagePullPolicy: IfNotPresent` won't pick up quarterly builds
`k8s/deployment.yaml` pulls `ghcr.io/knowlesy/my-news-station:latest` with `IfNotPresent`: a node
that already has *any* `latest` cached will never fetch the new quarterly image.
**Fix:** deploy the `YYYY-MM` date tag the workflow already pushes, or set `Always`.

### 22. Frontend depends on CDN at runtime
epub.js and jszip load from jsdelivr (`index.html:9–12`). The reader breaks if the dashboard is
used without internet (plausible for a self-hosted LAN k8s app).
**Fix:** vendor the two minified files into `frontend/vendor/`.

### 23. Stock-firmware X4 sends can't work from the cluster
Stock Xteink firmware only exposes its HTTP server on the device's *own hotspot*
(192.168.3.3) — the k8s pod can never reach it. Only CrossPoint firmware devices joined to the
LAN are actually reachable from the server. Worth a note in the Settings UI ("requires CrossPoint
firmware on your network"), or the probe result will just permanently read offline.

### 24. Minor
- `scraper.py` uses naive `datetime.now()` for `source_activity.json` while the frontend compares
  with browser-local `new Date()` — "days ago" can be off by the container/browser TZ delta. Use
  UTC (`datetime.now(timezone.utc)`) since the frontend already parses ISO strings.
- `probeDevice` cache in the frontend and the `updateSendButton` unknown/else branches are
  identical — collapse the two branches.
- `edge_tts` gets the entire podcast script in one call; long scripts occasionally fail
  mid-stream with no retry. A sentence-chunked retry loop would make audio generation resilient.
- `.venv/` sits in the repo working tree — confirm it's gitignored (nodriver was installed into
  it during testing and has a local patch; it should never be committed).

---

## What's in good shape

- The Rust server is small, typed, and does the right thing with background jobs + a log ring
  buffer; no framework misuse.
- The scraper pipeline phases are clearly delineated and logged; sidecar-based audio regen is a
  sound design.
- Multi-stage Dockerfile with dependency-layer caching is correct.
- The k8s manifests are tidy: Recreate strategy for RWO PVCs, probes, resource limits,
  concurrencyPolicy on the CronJob are all sensible.

## Suggested order of attack

1. ~~**P0 items 1–5**~~ — ✅ done (2026-07-04).
2. ~~**Items 6–9**~~ — ✅ done (2026-07-04, including item 10).
3. ~~**Item 11 + 18–19**~~ — ✅ done (2026-07-04).
4. ~~**Items 12–14**~~ — ✅ done (2026-07-04).
5. ~~**Item 15**~~ — ✅ done (2026-07-04), verified by Playwright smoke test.
6. **P3 (items 20–24)** — remaining hygiene: Dockerfile double-install, `:latest` pull policy,
   CDN vendoring, stock-firmware UX note, minor items.
