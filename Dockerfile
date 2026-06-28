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
FROM rust:1.79-slim-bookworm AS rust-builder

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
#   • Ubuntu Jammy (22.04)
#   • Python 3.11
#   • Node.js 18
#   • Chromium + all system dependencies
FROM mcr.microsoft.com/playwright/python:v1.44.0-jammy

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
    libasound2 \
    # Misc utilities
    curl \
    ca-certificates \
    && curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# ── Claude CLI (via npm — Node.js is pre-installed in the base image) ─────
# This installs the `claude` command globally.
# First-time OAuth: docker exec -it <container> claude
RUN npm install -g @anthropic-ai/claude-code 2>/dev/null \
    && echo "✓ Claude CLI installed: $(claude --version 2>/dev/null || echo 'check manually')"

# ── Python application ───────────────────────────────────────────
WORKDIR /app

# Copy and install Python dependencies
COPY scraper/requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt

# Install Playwright Chromium browser binaries
RUN playwright install chromium --with-deps

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
    PYTHONUNBUFFERED=1

EXPOSE 3000

# ── Entrypoint ────────────────────────────────────────────────────
# The web SERVER runs continuously (serving the dashboard).
# The SCRAPER is invoked on a schedule via Kubernetes CronJob.
#
# Xvfb is started first to provide a virtual display for Playwright;
# without it, even "headless" Chromium in some K8s environments fails.
CMD ["sh", "-c", \
     "Xvfb :99 -screen 0 1920x1080x24 -ac +extension GLX &>/dev/null & \
      sleep 1 && \
      echo '✓ Xvfb started on :99' && \
      exec /app/server"]
