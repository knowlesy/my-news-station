# ═══════════════════════════════════════════════════════════════════
# Daily News Media Station — Multi-Stage Dockerfile
#
# Stage 1  (rust-builder): Compile the Axum web server binary
# Stage 2  (final):        Runtime image with Python, Playwright,
#                          Xvfb, and the Claude CLI
#
# Claude CLI first-time auth:
#   docker exec -it <container> claude
#   → follow the URL → paste code → credentials cached to ~/.claude/
# ═══════════════════════════════════════════════════════════════════

# ── Stage 1: Compile Rust web server ────────────────────────────────────────
FROM rust:1.97-slim-bookworm AS rust-builder

WORKDIR /build

# Install C linker (required for some crates)
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy manifests first to cache dependency compilation layer
COPY server/Cargo.toml ./
# Stub main.rs so `cargo fetch` / dependency compile succeeds
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs

# Pre-compile dependencies (cached unless Cargo.toml changes)
RUN cargo build --release && rm -rf src

# Now copy and compile the real source
COPY server/src ./src
# Touch main.rs to invalidate the stub's build artifact
RUN touch src/main.rs && cargo build --release

# ── Stage 2: Runtime image ───────────────────────────────────────────────────
# microsoft/playwright/python ships:
#   • Ubuntu Noble (24.04)
#   • Python 3.12  ← the apt python3. Every dependency floor in
#                    requirements.txt must resolve on this version;
#                    local dev runs 3.14, so the two are not the same.
#                    (Was jammy/3.10, which capped scikit-learn at 1.7.x.)
#   • Node.js 18
#   • Chromium + all system dependencies
#
# The image removes /usr/lib/python3.12/EXTERNALLY-MANAGED, so PEP 668
# does not block `pip install` into the system interpreter below.
FROM mcr.microsoft.com/playwright/python:v1.61.0-noble

# Build identity — passed via --build-arg from CI, surfaced at /api/version
ARG GIT_SHA=dev
ARG BUILD_DATE=unknown

# ── System packages ──────────────────────────────────────────────
RUN apt-get update && apt-get install -y --no-install-recommends \
    # X Virtual Framebuffer — needed for Playwright in non-headless mode
    xvfb \
    # Additional Chromium system libraries (belt-and-suspenders)
    libnss3 \
    libatk-bridge2.0-0 \
    libdrm2 \
    libxcomposite1 \
    libxdamage1 \
    libxrandr2 \
    libgbm1 \
    # Ubuntu 24.04's 64-bit time_t transition renamed this; the old name is
    # now a virtual package with no install candidate and fails the build.
    libasound2t64 \
    # Misc utilities
    curl \
    ca-certificates \
    && curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# ── Claude CLI (via npm — Node.js is pre-installed in the base image) ─────
# This installs the `claude` command globally.
# First-time OAuth: docker exec -it <container> claude
RUN npm install -g @anthropic-ai/claude-code \
    && echo "✓ Claude CLI installed: $(claude --version)"

# ── Python application ───────────────────────────────────────────
WORKDIR /app

# Copy and install Python dependencies
COPY scraper/requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt

# Install the Chromium build matching the pip-resolved playwright version.
# (The base image bundles v1.44 browsers, but pip may resolve a newer
# playwright whose browser revision differs — so this stays version-matched.)
# --with-deps is deliberately omitted: the base image already has all system
# libraries, and reinstalling them was pure image bloat.
RUN playwright install chromium

# Copy application files
COPY scraper/   /app/scraper/
COPY frontend/  /app/frontend/

# Copy compiled Rust server binary from builder stage
COPY --from=rust-builder /build/target/release/server /app/server
RUN chmod +x /app/server

# ── Directory structure ───────────────────────────────────────────
# /app/data          → generated media files (EPUB + MP3) — mounted via PVC
# /root/.claude      → Claude CLI OAuth credentials — mounted via PVC
RUN mkdir -p /app/data /root/.claude

# ── Volume mount points ───────────────────────────────────────────
VOLUME ["/app/data", "/root/.claude"]

# ── Environment variables ─────────────────────────────────────────
ENV DATA_DIR=/app/data \
    FRONTEND_DIR=/app/frontend \
    BIND_ADDR=0.0.0.0:3000 \
    DISPLAY=:99 \
    # Default to Claude CLI backend; override with LLM_BACKEND=gemini or claude_api
    LLM_BACKEND=claude_cli \
    # Playwright: run Chromium headless inside Xvfb
    PLAYWRIGHT_BROWSERS_PATH=/ms-playwright \
    PYTHONUNBUFFERED=1 \
    GIT_SHA=${GIT_SHA} \
    BUILD_DATE=${BUILD_DATE}

EXPOSE 3000

# ── Entrypoint ────────────────────────────────────────────────────
# The web SERVER runs continuously (serving the dashboard).
# The SCRAPER is invoked on a schedule via Kubernetes CronJob.
#
# Xvfb provides a virtual display for Playwright; without it, even
# "headless" Chromium in some K8s environments fails. Under
# SCRAPE_RUNNER=k8s the server never forks Playwright — it only creates
# Jobs — so the display is dead weight and is skipped.
CMD ["sh", "-c", \
     "if [ \"$SCRAPE_RUNNER\" = \"k8s\" ]; then \
        echo '✓ SCRAPE_RUNNER=k8s — skipping Xvfb (server does not run Playwright)'; \
      else \
        Xvfb :99 -screen 0 1920x1080x24 -ac +extension GLX &>/dev/null & \
        sleep 1 && \
        echo '✓ Xvfb started on :99'; \
      fi && \
      exec /app/server"]
