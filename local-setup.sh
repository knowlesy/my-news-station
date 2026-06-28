#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════
# local-setup.sh — one-shot local development bootstrap (macOS)
#
# Run once:  bash local-setup.sh
# Then:      source .venv/bin/activate
#            cp .env.example .env   (add your GOOGLE_AI_KEY)
#            python scraper/scraper.py   # test scraper
#            cargo run --manifest-path server/Cargo.toml  # test server
# ═══════════════════════════════════════════════════════════════════
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$PROJECT_DIR"

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RESET='\033[0m'
info()  { echo -e "${GREEN}▶ $*${RESET}"; }
warn()  { echo -e "${YELLOW}⚠ $*${RESET}"; }

# ── 1. Python virtual environment ────────────────────────────────
info "Creating Python virtual environment at .venv …"
python3 -m venv .venv
source .venv/bin/activate

info "Installing Python dependencies …"
pip install --quiet --upgrade pip
pip install --quiet -r scraper/requirements.txt

# ── 2. Playwright browsers ───────────────────────────────────────
info "Installing Playwright Chromium browser …"
playwright install chromium

# ── 3. Data directory ────────────────────────────────────────────
info "Creating ./data directory …"
mkdir -p data

# ── 4. .env file ─────────────────────────────────────────────────
if [ ! -f .env ]; then
    info "Copying .env.example → .env"
    cp .env.example .env
    warn "Open .env and set GOOGLE_AI_KEY (or ANTHROPIC_API_KEY) before running the scraper"
else
    info ".env already exists — skipping copy"
fi

# ── 5. Cargo check (optional — only if you want the Rust server) ─
info "Checking Rust server compiles …"
cargo check --manifest-path server/Cargo.toml --quiet && \
    echo -e "${GREEN}  ✓ Rust server OK${RESET}" || \
    warn "Rust check failed — run 'cargo build' in ./server for details"

echo ""
echo -e "${GREEN}════════════════════════════════════════════════════${RESET}"
echo -e "${GREEN}  Setup complete!${RESET}"
echo ""
echo "  Next steps:"
echo "  1. Open .env and set your GOOGLE_AI_KEY"
echo "  2. source .venv/bin/activate"
echo "  3. LLM_BACKEND=gemini DATA_DIR=./data python scraper/scraper.py"
echo ""
echo "  To also run the web server:"
echo "  4. cd server && cargo run"
echo "     → open http://localhost:3000"
echo -e "${GREEN}════════════════════════════════════════════════════${RESET}"
