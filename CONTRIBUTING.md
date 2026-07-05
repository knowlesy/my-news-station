# Contributing

Issues and pull requests are welcome. This is a personal self-hosted project, so
scope is deliberately tight — open an issue to discuss bigger ideas before building.

## Development setup

```bash
bash local-setup.sh          # one-shot bootstrap (macOS): venv, deps, Playwright
cp .env.example .env         # add your LLM key
```

Run the pieces individually:

```bash
source .venv/bin/activate
python scraper/scraper.py                        # scraper pipeline
cargo run --manifest-path server/Cargo.toml      # web server
```

Or the whole stack: `docker compose up --build` (dashboard on http://localhost:3001).

## Before opening a PR

- **Build the image locally** — `docker build .` must succeed; CI builds are quarterly/manual, so a broken Dockerfile bites late
- **Run the tests** — `python -m pytest scraper/tests/` (`pip install pytest` if needed) and `cargo test --manifest-path server/Cargo.toml`
- **Test the behaviour you changed**, ideally inside the built image — this repo's history is full of commits verified that way, and it's the expected bar
- Keep commits focused, with messages that explain *why*

## Layout

| Path | What lives there |
|------|------------------|
| `scraper/` | Python pipeline: scrape → curate → LLM → EPUB/TTS |
| `server/` | Rust/Axum server: dashboard, media, config API, OPDS |
| `frontend/` | No-framework SPA (Catppuccin), epub.js readers |
| `k8s/` | Deployment, CronJob, PVC, Service |
