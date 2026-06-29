# My News Station

A self-hosted, automated daily news media station. Each morning the scraper pulls BBC News, curated DevOps/CI-CD blogs, Substack publications, and Medium sources, curates them with AI, and generates:

- 📖 A full **EPUB** daily newspaper (every article, Reader-Mode cleaned)
- 📻 A 30-minute **flash radio briefing** MP3
- 🎧 A long-form **podcast** MP3

All served through a beautiful, responsive **Catppuccin**-themed web dashboard with a built-in EPUB reader.

![Dashboard Screenshot](assets/screenshot-dashboard.png)

---

## Directory Structure

```
my-news/
├── scraper/
│   ├── scraper.py          # Main AI pipeline
│   └── requirements.txt
├── server/
│   ├── Cargo.toml
│   └── src/main.rs         # Axum web server
├── frontend/
│   └── index.html          # Catppuccin SPA dashboard
├── assets/                 # Documentation screenshots
├── data/                   # Generated media (PVC mount)
├── k8s/
│   ├── pvc.yaml            # Storage volumes
│   ├── deployment.yaml     # Server Deployment + Scraper CronJob
│   └── service.yaml        # LAN exposure
├── Dockerfile
└── .env.example
```

---

## LLM Backends

Set `LLM_BACKEND` to one of:

| Value        | Description                                | Required credentials         |
|-------------|---------------------------------------------|------------------------------|
| `gemini`     | Google AI Studio REST (Recommended)         | `GOOGLE_AI_KEY`              |
| `claude_cli` | Claude via OAuth (no API key needed)        | Run auth flow once (below)  |
| `claude_api` | Anthropic Python SDK                        | `ANTHROPIC_API_KEY`          |

---

## Quick Start (Docker Compose)

The easiest way to run the application is using `docker-compose`:

```bash
# 1. Copy template configuration
cp .env.example .env

# 2. Open .env and set your GOOGLE_AI_KEY
# 3. Start the services
docker compose up -d --build

# 4. Open the dashboard in your browser
open http://localhost:3000
```

---

## Claude CLI OAuth Setup (Alternative)

If using the `claude_cli` backend:

```bash
# 1. Exec into the running container
docker exec -it my-news-server claude

# 2. Open the printed authorization URL in your browser
# 3. Log in with your Claude.ai account, authorize the app, and paste the code back
```

---

## Kubernetes Deployment

```bash
# 1. Create namespace and apply resources
kubectl apply -f k8s/pvc.yaml
kubectl apply -f k8s/deployment.yaml
kubectl apply -f k8s/service.yaml

# 2. Set generic secret credentials
kubectl create secret generic news-secrets \
  --namespace my-news \
  --from-literal=LLM_BACKEND=gemini \
  --from-literal=GOOGLE_AI_KEY="your-google-ai-key"
```

---

## Customizing Sources

You can manage sources directly inside the web settings modal, which saves configuration to `data/config.json`. The scraper supports:
- **Standard RSS Feeds**
- **Medium tags** (e.g. `medium/tags/terraform` for tag feeds)
- **Medium User Profiles & Publications** (e.g. `@username` or custom domains)
- **Substack publications** (automatically resolves Substack links to their `/feed` endpoint)

---

## Dashboard Features

- **Catppuccin Theming**: Mocha (default), Macchiato, Frappé, Latte switching.
- **Dynamic New Badge & Expiration**: Shows green badges and unread edition counts for runs under 12 hours old, reverting to neutral once read.
- **Live Scraper Console**: Direct monospaced terminal logs modal inside Settings, with auto-scroll and status indicators.
- **Persistent URL Registry**: Deduplicates already-scraped articles inside `data/scraped_urls.json`, speeding up executions by up to 95% and saving Gemini/Claude tokens.
- **Optimized Code Rendering**: Monospaced code snippets and formatted CLI blocks render beautifully inside the built-in EPUB reader.

![EPUB Reader Code Block](assets/screenshot-code-block.png)

---

## Storage & Cleanup

The Rust server automatically deletes media files older than **10 days** every 6 hours to ensure persistent volumes do not run out of space.
