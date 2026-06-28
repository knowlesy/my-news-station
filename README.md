# My News Station

A self-hosted, automated daily news media station. Each morning the scraper pulls BBC News and Medium/terraform articles, curates them with AI, and generates:

- 📖 A full **EPUB** daily newspaper (every article, Reader-Mode cleaned)
- 📻 A 30-minute **flash radio briefing** MP3
- 🎧 A long-form **podcast** MP3

All served through a beautiful **Catppuccin**-themed web dashboard with a built-in EPUB reader.

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
| `claude_cli` | Claude via OAuth (no API key needed) ✅ default | Run auth flow once (below)  |
| `claude_api` | Anthropic Python SDK                        | `ANTHROPIC_API_KEY`          |
| `gemini`     | Google AI Studio REST                       | `GOOGLE_AI_KEY`              |

---

## Claude CLI OAuth Setup (One-Time)

The `claude_cli` backend uses the **Claude Code CLI** with cached OAuth credentials — no API key billing required.

### First time (after container starts):

```bash
# 1. Exec into the running container
docker exec -it my-news claude

# Or for Kubernetes:
kubectl exec -it -n my-news deploy/news-server -- claude
```

```
# 2. The CLI will print something like:
#    To sign in, open this URL in your browser:
#    https://claude.ai/oauth/authorize?...

# 3. Open the URL, log in with your Claude.ai account, authorise the app
# 4. Copy the one-time code shown in the browser
# 5. Paste it back into the terminal and press Enter

# ✓ Credentials are saved to /root/.claude/ (mounted PVC)
#   Subsequent scraper runs use the cached session automatically.
```

> **Note:** The claude-creds-pvc PVC keeps your tokens alive across pod restarts.
> You only need to run this once (or after the token expires, typically annually).

---

## Quick Start (Docker)

```bash
# 1. Build the image
docker build -t my-news:latest .

# 2. Run the server
docker run -d \
  --name my-news \
  -p 3000:3000 \
  -e LLM_BACKEND=claude_cli \
  -v my-news-data:/app/data \
  -v my-news-claude:/root/.claude \
  my-news:latest

# 3. Authenticate Claude CLI (first time only)
docker exec -it my-news claude

# 4. Open the dashboard
open http://localhost:3000

# 5. Manually trigger the scraper
docker exec -it my-news sh -c \
  "Xvfb :99 -screen 0 1920x1080x24 &>/dev/null & sleep 2 && python /app/scraper/scraper.py"
```

---

## Kubernetes Deployment

```bash
# 1. Build and push to your registry
docker build -t registry.local/my-news:latest .
docker push registry.local/my-news:latest

# 2. Update image: field in k8s/deployment.yaml to match

# 3. Create the namespace and apply all manifests
kubectl apply -f k8s/pvc.yaml
kubectl apply -f k8s/deployment.yaml
kubectl apply -f k8s/service.yaml

# 4. Create the secrets (fill in real values)
kubectl create secret generic news-secrets \
  --namespace my-news \
  --from-literal=LLM_BACKEND=claude_cli \
  --from-literal=GOOGLE_AI_KEY="" \
  --from-literal=ANTHROPIC_API_KEY=""

# 5. Authenticate Claude CLI (first time)
kubectl exec -it -n my-news deploy/news-server -- claude

# 6. Expose via NodePort (edit service.yaml) or port-forward
kubectl port-forward -n my-news svc/news-server 3000:80

# 7. Trigger a test scrape immediately
kubectl create job --from=cronjob/news-scraper test-scrape -n my-news
```

---

## Customising Sources

Edit `scraper/scraper.py`:

```python
# Add RSS feeds
RSS_FEEDS = [
    {"name": "BBC News",  "url": "http://feeds.bbci.co.uk/news/rss.xml"},
    {"name": "Reuters",   "url": "https://feeds.reuters.com/reuters/topNews"},
]

# Add Medium tags
MEDIUM_TAGS = ["terraform", "devops", "kubernetes"]
```

---

## Dashboard Features

- **4 Catppuccin themes**: Mocha (default), Macchiato, Frappé, Latte — persisted across sessions
- **Date chip selector** — auto-selects the most recent edition
- **Dual audio players** — with animated progress fill bars and MP3 download buttons
- **Built-in EPUB reader** — epub.js with prev/next chapter navigation and keyboard arrow key support
- **Keyboard navigation** — `←` / `→` arrow keys for chapter browsing

---

## Storage & Cleanup

The Rust server automatically deletes media files older than **10 days** every 6 hours.  
The `news-data-pvc` PVC is provisioned at 20Gi to comfortably hold a rolling 10-day window.
