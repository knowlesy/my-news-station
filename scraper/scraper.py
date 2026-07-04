#!/usr/bin/env python3
"""
╔══════════════════════════════════════════════════════════════════╗
║          Daily News Media Station — AI Orchestrator             ║
║                                                                  ║
║  Backends:  gemini | claude_api | claude_cli                     ║
║  Claude CLI OAuth setup:                                         ║
║    docker exec -it <container> claude                            ║
║    → follow URL → paste code → credentials cached to ~/.claude/  ║
╚══════════════════════════════════════════════════════════════════╝
"""

import asyncio
import json
import logging
import os
import re
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

import edge_tts
import feedparser
import requests
import trafilatura
from bs4 import BeautifulSoup
from ebooklib import epub
from playwright.async_api import async_playwright
from playwright_stealth import Stealth
from sklearn.feature_extraction.text import TfidfVectorizer
from sklearn.metrics.pairwise import cosine_similarity

# ═══════════════════════════════════════════════════════════════════
# CONFIGURATION — override all values via environment variables
# ═══════════════════════════════════════════════════════════════════

DATA_DIR = Path(os.getenv("DATA_DIR", "/app/data"))
COOKIES_PATH = Path(os.getenv("COOKIES_PATH", "/app/cookies.json"))

# ── LLM backend selection ────────────────────────────────────────
#   gemini      → Google AI Studio REST (GOOGLE_AI_KEY)
#   claude_api  → Anthropic Python SDK  (ANTHROPIC_API_KEY)
#   claude_cli  → `claude` CLI OAuth    (cached ~/.claude/ creds)
LLM_BACKEND = os.getenv("LLM_BACKEND", "claude_cli")

GOOGLE_AI_KEY   = os.getenv("GOOGLE_AI_KEY", "")
ANTHROPIC_API_KEY = os.getenv("ANTHROPIC_API_KEY", "")
CLAUDE_MODEL    = os.getenv("CLAUDE_MODEL", "claude-opus-4-5")
GEMINI_MODEL    = os.getenv("GEMINI_MODEL", "gemini-2.5-flash")

# ── News sources ─────────────────────────────────────────────────
# Sources are OWNED by config.json (data dir). The web server materialises the
# canonical defaults there on first boot; the scraper has no embedded list.
# Both are populated by load_config(), called from the entry points below.
RSS_FEEDS: list[dict] = []
MEDIUM_TAGS: list[str] = []

# Sources whose article pages hard-block automated fetches at the network
# level (Akamai edge, in Daily Mail's case) regardless of browser fingerprint —
# skip the full-page fetch and use the RSS summary as the content instead.
NO_BROWSER_FETCH_SOURCES = {"Daily Mail"}

# ── Tuning ───────────────────────────────────────────────────────
TOP_N                = 10     # Max articles per source
SIMILARITY_THRESHOLD = 0.6    # TF-IDF cosine sim → "High-Impact Highlight"
TOP_MEDIUM_AUDIO     = 3      # Top Medium posts forced into audio track
EXTRACT_CONCURRENCY  = int(os.getenv("EXTRACT_CONCURRENCY", "6"))  # Max simultaneous browser pages
FEED_TIMEOUT_SECS    = 15     # Per-feed HTTP timeout for RSS fetches

# ── Browser fingerprint ──────────────────────────────────────────
USER_AGENT = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
    "AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/125.0.0.0 Safari/537.36"
)

# ── TTS voices ───────────────────────────────────────────────────
VOICE_SHORT = os.getenv("VOICE_SHORT", "en-GB-SoniaNeural")   # Flash briefing — crisp British female
VOICE_LONG  = os.getenv("VOICE_LONG", "en-US-GuyNeural")     # Long podcast — warm American male

# ═══════════════════════════════════════════════════════════════════
# LOGGING
# ═══════════════════════════════════════════════════════════════════

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(name)s — %(message)s",
    handlers=[logging.StreamHandler(sys.stdout)],
)
log = logging.getLogger("news-station")

# ═══════════════════════════════════════════════════════════════════
# JSON FILE HELPERS & CONFIGURATION LOADING
# ═══════════════════════════════════════════════════════════════════

CONFIG_PATH = DATA_DIR / "config.json"


def load_json(path: Path, default):
    """Read a JSON file, returning `default` if it is missing or unparseable."""
    if not path.exists():
        return default
    try:
        with open(path, "r", encoding="utf-8") as f:
            return json.load(f)
    except Exception as e:
        log.warning("Failed to read %s: %s", path, e)
        return default


def save_json(path: Path, obj) -> None:
    """Write an object as pretty-printed JSON, logging (not raising) on failure."""
    try:
        with open(path, "w", encoding="utf-8") as f:
            json.dump(obj, f, ensure_ascii=False, indent=2, default=str)
    except Exception as e:
        log.warning("Failed to write %s: %s", path, e)


def _normalise_feed(item) -> dict | None:
    """Coerce a config feed entry (dict or bare URL string) into {name, url}."""
    url = item.get("url", "") if isinstance(item, dict) else item
    if not isinstance(url, str) or not url:
        return None

    # Auto-normalize Substack URLs to RSS feeds
    if ".substack.com" in url.lower() and not url.lower().rstrip("/").endswith("/feed"):
        url = url.rstrip("/") + "/feed"

    if isinstance(item, dict):
        return {**item, "url": url}
    from urllib.parse import urlparse
    domain = urlparse(url).netloc or "News Feed"
    return {"name": domain.replace("www.", ""), "url": url}


def load_config() -> None:
    """
    Populate RSS_FEEDS / MEDIUM_TAGS / SYSTEM_PROMPT from config.json.

    The web server owns the canonical defaults and writes config.json on its
    first boot, so a missing file means the server has never run against this
    data directory — fail loudly rather than silently scraping nothing.
    """
    global RSS_FEEDS, MEDIUM_TAGS, SYSTEM_PROMPT

    cfg = load_json(CONFIG_PATH, None)
    if cfg is None:
        raise SystemExit(
            f"config.json not found at {CONFIG_PATH}. Start the web server once "
            f"(it writes the default config), or create the file manually."
        )

    RSS_FEEDS = [
        feed for item in cfg.get("rss_feeds", [])
        if (feed := _normalise_feed(item)) is not None
    ]
    MEDIUM_TAGS = list(cfg.get("medium_tags") or [])

    # Drop silenced sources entirely — no scrape, no extraction, no EPUB
    silenced = set(cfg.get("silenced_sources") or [])
    if silenced:
        before = len(RSS_FEEDS) + len(MEDIUM_TAGS)
        RSS_FEEDS = [f for f in RSS_FEEDS if f.get("name") not in silenced]
        # UI labels Medium sources as "Medium/tags/<tag>"; accept the raw tag too
        MEDIUM_TAGS = [
            t for t in MEDIUM_TAGS
            if f"Medium/tags/{t}" not in silenced and t not in silenced
        ]
        log.info("Silenced %d source(s) via config", before - len(RSS_FEEDS) - len(MEDIUM_TAGS))

    if cfg.get("system_prompt"):
        SYSTEM_PROMPT = cfg["system_prompt"]
        log.info("Loaded custom system prompt from config (%d chars)", len(SYSTEM_PROMPT))

    log.info("Loaded sources config: %d RSS feeds, %d Medium tags",
             len(RSS_FEEDS), len(MEDIUM_TAGS))

# ═══════════════════════════════════════════════════════════════════
# LLM BACKENDS
# ═══════════════════════════════════════════════════════════════════

def call_gemini(prompt: str) -> str:
    """
    Call Google Gemini via the generativelanguage REST API.
    Requires: GOOGLE_AI_KEY environment variable.
    Free key available at: https://aistudio.google.com
    """
    if not GOOGLE_AI_KEY:
        raise ValueError("GOOGLE_AI_KEY is not set. Get a free key at https://aistudio.google.com")

    url = (
        f"https://generativelanguage.googleapis.com/v1beta/models/"
        f"{GEMINI_MODEL}:generateContent?key={GOOGLE_AI_KEY}"
    )
    payload = {
        "contents": [{"parts": [{"text": prompt}]}],
        "generationConfig": {
            "maxOutputTokens": 16384,
            "temperature": 0.4,
        },
    }
    log.info("→ Calling Gemini (%s)…", GEMINI_MODEL)
    resp = requests.post(url, json=payload, timeout=180)
    if resp.status_code != 200:
        log.error("Gemini API Error Response Body: %s", resp.text)
    if resp.status_code == 404:
        raise RuntimeError(
            f"Gemini model '{GEMINI_MODEL}' not found (404).\n"
            f"'gemini-1.5-pro' is deprecated — use 'gemini-2.0-flash'.\n"
            f"Set GEMINI_MODEL=gemini-2.0-flash in your .env file."
        )
    resp.raise_for_status()
    return resp.json()["candidates"][0]["content"]["parts"][0]["text"]


def call_claude_api(prompt: str) -> str:
    """
    Call Anthropic Claude via the official Python SDK.
    Requires: pip install anthropic
              ANTHROPIC_API_KEY environment variable.
    API keys: https://console.anthropic.com
    """
    try:
        import anthropic
    except ImportError:
        raise ImportError("Install the Anthropic SDK: pip install anthropic")

    if not ANTHROPIC_API_KEY:
        raise ValueError("ANTHROPIC_API_KEY is not set. Get one at https://console.anthropic.com")

    client = anthropic.Anthropic(api_key=ANTHROPIC_API_KEY)
    log.info("→ Calling Claude API (%s)…", CLAUDE_MODEL)
    message = client.messages.create(
        model=CLAUDE_MODEL,
        max_tokens=16384,
        messages=[{"role": "user", "content": prompt}],
    )
    return message.content[0].text


def call_claude_cli(prompt: str) -> str:
    """
    Call Claude via the `claude` CLI tool using cached OAuth credentials.

    ┌──────────────────────────────────────────────────────────┐
    │  FIRST-TIME SETUP (one-off, then credentials are cached)  │
    │                                                           │
    │  1.  docker exec -it <your-container-name> bash           │
    │  2.  claude                                               │
    │  3.  Select "Claude.ai (free / Pro)" login option         │
    │  4.  Visit the printed URL in your browser                │
    │  5.  Authorise → copy the one-time code                   │
    │  6.  Paste the code back in the terminal and press Enter  │
    │                                                           │
    │  Credentials are saved to /root/.claude/                  │
    │  (mounted as a PVC so they survive pod restarts)          │
    └──────────────────────────────────────────────────────────┘
    """
    log.info("→ Calling Claude CLI (cached OAuth)…")

    result = subprocess.run(
        [
            "claude",
            "--dangerously-skip-permissions",   # allow non-interactive
            "-p", prompt,
            "--output-format", "text",
        ],
        capture_output=True,
        text=True,
        timeout=360,
        env={**os.environ, "NO_COLOR": "1"},    # suppress ANSI codes in output
    )

    if result.returncode != 0:
        stderr_snippet = result.stderr[:800] if result.stderr else "(no stderr)"
        raise RuntimeError(
            f"claude CLI exited {result.returncode}.\n"
            f"Hint: run `docker exec -it <container> claude` to complete OAuth setup.\n"
            f"stderr: {stderr_snippet}"
        )

    output = result.stdout.strip()
    if not output:
        raise RuntimeError("claude CLI returned an empty response")

    return output


def call_llm(prompt: str) -> str:
    """Route to the configured LLM backend (LLM_BACKEND env var)."""
    backends = {
        "gemini":      call_gemini,
        "claude_api":  call_claude_api,
        "claude_cli":  call_claude_cli,
    }
    if LLM_BACKEND not in backends:
        raise ValueError(
            f"Unknown LLM_BACKEND={LLM_BACKEND!r}. "
            f"Valid options: {list(backends.keys())}"
        )
    return backends[LLM_BACKEND](prompt)


# ═══════════════════════════════════════════════════════════════════
# STEALTH BROWSER LAYER
# ═══════════════════════════════════════════════════════════════════

async def make_browser_context(playwright):
    """
    Launch a stealth-patched Chromium context with a realistic digital fingerprint.
    playwright-stealth patches: navigator.webdriver, plugins, languages, Canvas, WebGL, etc.
    Cookies from COOKIES_PATH are injected to simulate a logged-in session.
    """
    browser = await playwright.chromium.launch(
        headless=True,
        args=[
            "--no-sandbox",
            "--disable-setuid-sandbox",
            "--disable-blink-features=AutomationControlled",
            "--disable-infobars",
            "--window-size=1920,1080",
            "--disable-dev-shm-usage",
            "--disable-gpu",
        ],
    )

    context = await browser.new_context(
        viewport={"width": 1920, "height": 1080},
        user_agent=USER_AGENT,
        locale="en-GB",
        timezone_id="Europe/London",
        extra_http_headers={
            "Accept-Language":    "en-GB,en;q=0.9",
            "Accept-Encoding":    "gzip, deflate, br",
            "Sec-CH-UA":          '"Chromium";v="125", "Google Chrome";v="125"',
            "Sec-CH-UA-Mobile":   "?0",
            "Sec-CH-UA-Platform": '"Windows"',
        },
    )

    # Load persisted session cookies to bypass login walls
    if COOKIES_PATH.exists():
        log.info("Loading session cookies from %s", COOKIES_PATH)
        with open(COOKIES_PATH) as f:
            cookies = json.load(f)
        await context.add_cookies(cookies)
    else:
        log.info("No cookies.json found at %s — proceeding without session", COOKIES_PATH)

    return browser, context


async def fetch_rendered_html(context, url: str) -> str:
    """
    Open a new stealth page, navigate to URL, wait for JS hydration,
    then return the full rendered HTML. Stealth patches are applied per-page.
    """
    page = await context.new_page()
    # playwright-stealth 2.x: patch fingerprint directly on the page object
    await Stealth().apply_stealth_async(page)

    try:
        await page.goto(url, wait_until="domcontentloaded", timeout=30_000)
        # Allow time for JS frameworks to finish rendering
        await page.wait_for_timeout(2_500)
        html = await page.content()
    except Exception as exc:
        log.warning("Failed to render %s: %s", url, exc)
        html = ""
    finally:
        await page.close()

    return html



# ═══════════════════════════════════════════════════════════════════
# RSS INGESTION
# ═══════════════════════════════════════════════════════════════════

def scrape_rss(feed: dict) -> list[dict]:
    """Parse an RSS feed and return the top TOP_N article stubs."""
    log.info("Parsing RSS: %s (%s)", feed["name"], feed["url"])
    # Fetch with an explicit timeout — feedparser's own fetching has none, and
    # a single blackholed feed would otherwise stall the whole pipeline.
    try:
        resp = requests.get(
            feed["url"],
            timeout=FEED_TIMEOUT_SECS,
            headers={"User-Agent": USER_AGENT},
        )
        resp.raise_for_status()
    except requests.RequestException as exc:
        log.warning("Failed to fetch RSS %s (%s): %s", feed["name"], feed["url"], exc)
        return []
    parsed = feedparser.parse(resp.content)

    articles = []
    for entry in parsed.entries[:TOP_N]:
        articles.append({
            "source":  feed["name"],
            "title":   entry.get("title", "Untitled").strip(),
            "url":     entry.get("link", ""),
            "summary": BeautifulSoup(
                entry.get("summary", ""), "html.parser"
            ).get_text(strip=True)[:500],
            "content": "",
            "author":  "",
            "images":  [],
            "audio_highlight": False,
        })

    log.info("  ↳ %d articles from %s", len(articles), feed["name"])
    return articles


# ═══════════════════════════════════════════════════════════════════
# MEDIUM INGESTION (TAGS, HANDLES & PUBLICATIONS)
# ═══════════════════════════════════════════════════════════════════

async def scrape_medium_source(context, source_input: str) -> list[dict]:
    """
    Navigate to a Medium page (tag, user profile, or publication),
    parse article cards, and return the top TOP_N articles.
    Supports tags, user profiles (@username), and publications (URLs or domains).
    """
    source_input = source_input.strip()
    if not source_input:
        return []

    # Determine URL and label
    if source_input.startswith("@"):
        url = f"https://medium.com/{source_input}"
        source_label = f"Medium/{source_input}"
    elif source_input.startswith("http://") or source_input.startswith("https://"):
        url = source_input
        from urllib.parse import urlparse
        parsed = urlparse(source_input)
        path_clean = parsed.path.strip("/")
        source_label = f"Medium/{parsed.netloc.replace('www.', '')}"
        if path_clean:
            source_label += f"/{path_clean}"
    elif "." in source_input:
        url = f"https://{source_input}"
        source_label = f"Medium/{source_input}"
    else:
        url = f"https://medium.com/tag/{source_input}"
        source_label = f"Medium/tags/{source_input}"

    log.info("Scraping Medium source: %s → %s", source_label, url)
    html = await fetch_rendered_html(context, url)

    if not html:
        log.warning("Got empty HTML for Medium source: %s", source_input)
        return []

    soup = BeautifulSoup(html, "html.parser")
    articles = []
    seen_urls: set[str] = set()

    for a_tag in soup.find_all("a", href=True):
        href: str = a_tag["href"]

        # Medium post URLs end with a 10–12 character hex hash after the last hyphen
        is_post_url = re.search(r"-[a-f0-9]{8,}(?:\?.*)?$", href)
        if not is_post_url:
            continue

        # Normalise URL
        if href.startswith("http"):
            full_url = href.split("?")[0]
        elif href.startswith("/"):
            from urllib.parse import urlparse
            base_netloc = urlparse(url).netloc
            full_url = f"https://{base_netloc}" + href.split("?")[0]
        else:
            continue

        if full_url in seen_urls:
            continue
        seen_urls.add(full_url)

        # Extract the nearest heading as the title
        title_el = a_tag.find(["h1", "h2", "h3"])
        if title_el:
            title = title_el.get_text(strip=True)
        else:
            raw_text = a_tag.get_text(strip=True)
            title = raw_text[:120] if len(raw_text) > 10 else ""

        if not title or len(title) < 10:
            continue

        articles.append({
            "source":          source_label,
            "title":           title,
            "url":             full_url,
            "summary":         "",
            "content":         "",
            "author":          "",
            "images":          [],
            "audio_highlight": False,
        })

        if len(articles) >= TOP_N:
            break

    log.info("  ↳ %d articles from %s", len(articles), source_label)
    return articles


def is_valid_author(name: str) -> bool:
    """Check if the extracted author is a valid person/entity name, filtering out URLs or social pages."""
    if not name:
        return False
    name_lower = name.lower().strip()
    if name_lower.startswith("http://") or name_lower.startswith("https://"):
        return False
    if any(k in name_lower for k in ["facebook.com", "twitter.com", "instagram.com", "x.com", "linkedin.com"]):
        return False
    # If the text has slashes, it might be a URL path
    if "/" in name_lower or "\\" in name_lower:
        return False
    if len(name.strip()) > 80:
        return False
    return True


def extract_author(html: str) -> str:
    """Extract the author name from HTML metadata or standard tags."""
    if not html:
        return ""
    try:
        soup = BeautifulSoup(html, "html.parser")
        
        # 1. Try meta name="author"
        meta_author = soup.find("meta", attrs={"name": "author"})
        if meta_author and meta_author.get("content"):
            val = meta_author["content"].strip()
            if is_valid_author(val):
                return val
            
        # 2. Try meta property="article:author"
        meta_art_author = soup.find("meta", attrs={"property": "article:author"})
        if meta_art_author and meta_art_author.get("content"):
            val = meta_art_author["content"].strip()
            if is_valid_author(val):
                return val
            
        # 3. Try standard Medium author testids/classes
        author_el = soup.find("a", attrs={"data-testid": "authorName"})
        if author_el:
            val = author_el.get_text(strip=True)
            if is_valid_author(val):
                return val
            
        # 4. Try any link with rel="author"
        rel_author = soup.find(attrs={"rel": "author"})
        if rel_author:
            val = rel_author.get_text(strip=True)
            if is_valid_author(val):
                return val
    except Exception:
        pass
    return ""


def extract_images(html: str) -> list[str]:
    """Extract up to 3 prominent image URLs from the HTML content."""
    if not html:
        return []
    images = []
    try:
        soup = BeautifulSoup(html, "html.parser")
        for img in soup.find_all("img", src=True):
            src = img["src"]
            if not src.startswith("http"):
                continue
            # Skip tracking pixels, avatars, or relative icons
            if any(k in src.lower() for k in ["avatar", "logo", "icon", "profile", "tracker", "pixel", "ad", "spacer"]):
                continue
            if src in images:
                continue
            images.append(src)
            if len(images) >= 3:
                break
    except Exception:
        pass
    return images


def format_text_to_html(text: str) -> str:
    """
    Format plain text/markdown into well-formed XHTML.
    Protects code blocks fenced with ``` and escapes special characters to prevent parser errors.
    """
    if not text:
        return "<p>(No content available)</p>"

    import html
    
    # Split by fenced code blocks
    parts = text.split("```")
    html_blocks = []
    
    for idx, part in enumerate(parts):
        # Odd indices are fenced code blocks
        if idx % 2 != 0:
            lines = part.split("\n")
            # If the first line is a language name, discard it or use it as class
            first_line = lines[0].strip().lower()
            if first_line in ["python", "bash", "sh", "json", "yaml", "yml", "terraform", "hcl", "dockerfile", "javascript", "js", "html", "css"]:
                code_content = "\n".join(lines[1:])
            else:
                code_content = part
            
            escaped_code = html.escape(code_content.strip())
            html_blocks.append(f"<pre><code>{escaped_code}</code></pre>")
        else:
            # Regular text paragraphs — split on single newlines to avoid wall of text
            paragraphs = [p.strip() for p in part.split("\n") if p.strip()]
            for para in paragraphs:
                escaped_para = html.escape(para)
                if escaped_para.startswith("## "):
                    html_blocks.append(f"<h3>{escaped_para[3:]}</h3>")
                elif escaped_para.startswith("# "):
                    html_blocks.append(f"<h2>{escaped_para[2:]}</h2>")
                else:
                    # Apply inline styles
                    escaped_para = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", escaped_para)
                    escaped_para = re.sub(r"\*(.+?)\*",     r"<em>\1</em>", escaped_para)
                    escaped_para = re.sub(r"`(.+?)`", r"<code>\1</code>", escaped_para)
                    html_blocks.append(f"<p>{escaped_para}</p>")
                
    return "\n".join(html_blocks)


# ═══════════════════════════════════════════════════════════════════
# CONTENT EXTRACTION (Reader Mode via trafilatura)
# ═══════════════════════════════════════════════════════════════════

def is_paywalled_content(content: str) -> bool:
    """Detect if article content contains paywall indicators (truncated/paywalled)."""
    if not content:
        return False

    content_lower = content.lower()
    paywall_patterns = [
        "sign up to read",
        "subscribe to continue",
        "subscribe to read",
        "subscribe for",
        "unlock this story",
        "unlock this article",
        "member-only story",
        "member-only article",
        "members only",
        "membership required",
        "support our journalism",
        "subscribe for just",
        "sign up for free",
        "read full article",
        "read the rest",
        "continue reading",
        "upgrade to read",
    ]
    return any(pattern in content_lower for pattern in paywall_patterns)


async def extract_article_content(context, article: dict) -> dict:
    """
    Render the article URL with the stealth browser and pass the full HTML
    to trafilatura's Reader-Mode extractor to get clean, junk-free text.
    Falls back to the RSS summary if extraction fails.
    """
    url = article["url"]
    if not url or article.get("source") in NO_BROWSER_FETCH_SOURCES:
        article["content"] = article.get("summary", "")
        return article

    html = await fetch_rendered_html(context, url)

    if html:
        # Preprocess HTML to protect <pre> code blocks from being mangled by trafilatura
        try:
            from bs4 import BeautifulSoup
            soup = BeautifulSoup(html, "html.parser")
            for pre in soup.find_all("pre"):
                code_text = pre.get_text()
                # Wrap in markdown fences and replace pre block with a text element
                placeholder = soup.new_string(f"\n\n```\n{code_text}\n```\n\n")
                pre.replace_with(placeholder)
            html = str(soup)
        except Exception as e:
            log.warning("BeautifulSoup code block preprocessing failed: %s", e)

        # trafilatura v2 dropped `no_fallback` and `favor_precision`; support both versions
        try:
            extracted = trafilatura.extract(
                html,
                include_comments=False,
                include_tables=True,
                include_formatting=True,
                no_fallback=False,
                favor_precision=True,
            )
        except TypeError:
            # v2.x API — fewer kwargs accepted
            extracted = trafilatura.extract(
                html,
                include_comments=False,
                include_tables=True,
                include_formatting=True,
            )
        article["content"] = extracted or article.get("summary", f"[Content unavailable: {url}]")
        article["is_paywalled"] = is_paywalled_content(article["content"])
        if article["is_paywalled"]:
            log.info("  ↳ Detected paywalled content: %s", article.get("title", url)[:60])
        article["author"] = extract_author(html)
        article["images"] = extract_images(html)
    else:
        article["content"] = article.get("summary", f"[Failed to fetch: {url}]")

    return article


# ═══════════════════════════════════════════════════════════════════
# AUDIO CURATION — TF-IDF CLUSTERING
# ═══════════════════════════════════════════════════════════════════

def curate_audio_highlights(all_articles: list[dict]) -> list[dict]:
    """
    Pass 1 — Breaking-news cluster detection:
      Build a TF-IDF matrix of all headlines. Any article whose
      maximum cosine similarity to another article exceeds SIMILARITY_THRESHOLD
      is tagged as a 'High-Impact Highlight' (breaking news appearing across
      multiple sources).

    Pass 2 — Top Medium picks:
      Force the top TOP_MEDIUM_AUDIO Medium articles into the audio track
      regardless of their similarity score.
    """
    if len(all_articles) < 2:
        log.warning("Not enough articles for TF-IDF curation")
        return all_articles

    headlines = [a["title"] for a in all_articles]

    try:
        vec = TfidfVectorizer(stop_words="english", ngram_range=(1, 2), min_df=1)
        tfidf_matrix = vec.fit_transform(headlines)
        sim_matrix = cosine_similarity(tfidf_matrix)
    except Exception as exc:
        log.warning("TF-IDF vectorisation failed: %s", exc)
        return all_articles

    # Pass 1: cross-source similarity highlights
    for i, article in enumerate(all_articles):
        row = sim_matrix[i].copy()
        row[i] = 0.0  # exclude self-similarity
        max_sim = float(row.max())
        is_highlight = max_sim > SIMILARITY_THRESHOLD
        article["audio_highlight"] = is_highlight
        article["max_sim"] = round(max_sim, 3)
        if is_highlight:
            log.info(
                "  [HIGHLIGHT sim=%.2f] %s",
                max_sim, article["title"][:80]
            )

    # Pass 2: top Medium picks always go to audio
    medium_articles = [a for a in all_articles if a.get("source", "").startswith("Medium/")]
    for a in medium_articles[:TOP_MEDIUM_AUDIO]:
        if not a["audio_highlight"]:
            a["audio_highlight"] = True
            log.info("  [MEDIUM PICK] %s", a["title"][:80])

    # Pass 3: Enforce limit of at most 4 highlights per news provider (chronological priority)
    MAX_HIGHLIGHTS_PER_SOURCE = 4
    source_counts = {}
    for a in all_articles:
        if a.get("audio_highlight"):
            source = a.get("source", "")
            count = source_counts.get(source, 0)
            if count >= MAX_HIGHLIGHTS_PER_SOURCE:
                a["audio_highlight"] = False
                log.info("  [HIGHLIGHT CAPPED (Demoted)] %s (%s)", a["title"][:80], source)
            else:
                source_counts[source] = count + 1

    total_highlights = sum(1 for a in all_articles if a["audio_highlight"])
    log.info("Audio highlights selected: %d / %d", total_highlights, len(all_articles))
    return all_articles


# ═══════════════════════════════════════════════════════════════════
# MEGA-PROMPT CONSTRUCTION
# ═══════════════════════════════════════════════════════════════════

DEFAULT_SYSTEM_PROMPT = """\
You are an advanced news editing and broadcasting engine with a distinct personality. You are provided with a complete daily pool of articles, along with a marked list of curated audio highlights. Process this text and wrap your outputs in these designated XML tags:

TONE RULES (apply to both outputs):
- For tech, business, and general news: be dry, witty, and a little sardonic. You have opinions. If a company has released its fifth "revolutionary" AI product this month, you may note that. If a framework has broken its API again, you can say so. Keep it sharp but never mean-spirited.
- For stories involving death, serious injury, war, disaster, mental health, or human tragedy: drop the wit entirely. Shift to a calm, respectful, measured tone. Acknowledge the weight of the story before moving on. Never make light of suffering.
- The tonal shift should feel deliberate and human — like a presenter who knows when to be funny and when to shut up and be decent.

- <short_radio>: Focus ONLY on the curated stories listed under the 'For <short_radio>' section below. Do NOT mention, summarize, or draw details from any other articles in the pool. Deliver a punchy flash briefing — think smart morning radio, not a press release.
  CRITICAL TIME CONSTRAINT: This script MUST be crisp and tightly edited. It must absolutely NOT exceed a 30-minute reading time under any circumstances. Target a high-density delivery between 500 to 2,000 words total.
  Structure: Continuous text script. Group stories by source and introduce them naturally (e.g., 'From BBC News...' or 'Over at GitHub...') instead of repeating '[Source] reported that' for each story. Keep it flowing and conversational. NO markdown formatting, asterisks, or bolding. Translate code/terminal commands into conceptual, easy-to-understand audio explanations.
  Always use time-neutral greetings (e.g., 'Hello', 'Welcome', 'Right then — here is what happened') rather than 'Good morning' or 'Good evening'.

- <long_podcast>: Focus ONLY on the curated stories listed under the 'For <long_podcast>' section below. Do NOT mention, summarize, or draw details from any other articles in the pool. Act as a sharp, conversational tech podcast host — one who has read the room, done the reading, and isn't afraid to say what they actually think. Seamless monologue, smooth transitions, no speaker tags or audio cues. Pure text prose optimized for TTS.
  Always use time-neutral greetings (e.g., 'Hello', 'Welcome back', 'Here we go again') rather than 'Good morning' or 'Good evening'.
"""

# Runtime value; load_config() replaces this with the config.json prompt if set
SYSTEM_PROMPT = DEFAULT_SYSTEM_PROMPT


def build_prompt(all_articles: list[dict]) -> str:
    """Assemble the single mega-prompt with the filtered article pool."""
    short_sources_list = [s.strip().lower() for s in os.getenv("SHORT_SOURCES", "BBC News").split(",") if s.strip()]
    long_sources_list = [s.strip().lower() for s in os.getenv("LONG_SOURCES", "BBC News,Medium/tags/terraform").split(",") if s.strip()]

    short_highlights = [a for a in all_articles if a.get("audio_highlight") and a.get("source", "").lower() in short_sources_list]
    long_highlights = [a for a in all_articles if a.get("audio_highlight") and a.get("source", "").lower() in long_sources_list]

    short_summary = "\n".join(f"  • [{a['source']}] {a['title']}" for a in short_highlights) or "  (none flagged)"
    long_summary = "\n".join(f"  • [{a['source']}] {a['title']}" for a in long_highlights) or "  (none flagged)"

    # Only pass articles to the LLM that are actually flagged as highlights to prevent leakage
    pool_sections = []
    highlight_articles = [a for a in all_articles if a.get("audio_highlight")]
    for i, a in enumerate(highlight_articles, 1):
        pool_sections.append(
            f"=== ARTICLE {i} ===\n"
            f"Source:  {a['source']}\n"
            f"Title:   {a['title']}\n"
            f"URL:     {a['url']}\n\n"
            f"{a.get('content') or a.get('summary', '(no content)')}\n"
        )

    return "\n\n".join([
        SYSTEM_PROMPT,
        f"── CRITICAL CONTENT FILTER RULES ──\n"
        f"- For <short_radio>: You must ONLY cover the following curated stories (from {os.getenv('SHORT_SOURCES', 'BBC News')}):\n"
        f"{short_summary}\n\n"
        f"- For <long_podcast>: You must ONLY cover the following curated stories (from {os.getenv('LONG_SOURCES', 'BBC News,Medium/tags/terraform')}):\n"
        f"{long_summary}\n",
        f"── FULL DAILY POOL (FOR DETAILS) ──",
        "\n\n".join(pool_sections),
    ])


# ═══════════════════════════════════════════════════════════════════
# XML RESPONSE PARSER
# ═══════════════════════════════════════════════════════════════════

def extract_xml_block(text: str, tag: str) -> str:
    """
    Extract the content of a single XML-wrapped block from the LLM response.
    Handles multi-line content and case-insensitive tag matching.
    """
    pattern = rf"<{tag}>(.*?)</{tag}>"
    match = re.search(pattern, text, re.DOTALL | re.IGNORECASE)
    if not match:
        log.warning("Could not find <%s>…</%s> block in LLM response", tag, tag)
        return ""
    return match.group(1).strip()


# ═══════════════════════════════════════════════════════════════════
# EPUB BUILDER
# ═══════════════════════════════════════════════════════════════════

EPUB_CSS = """
body {
    font-family: Georgia, 'Times New Roman', serif;
    line-height: 1.8;
    margin: 2.5em 3em;
    color: #1a1a2e;
    background: #fafafa;
}
pre {
    background-color: #f6f8fa;
    color: #24292e;
    padding: 1em;
    border-radius: 6px;
    border: 1px solid #e1e4e8;
    overflow-x: auto;
    font-family: 'Courier New', Courier, monospace;
    font-size: 0.85em;
    line-height: 1.45;
    margin: 1.5em 0;
}
code {
    font-family: 'Courier New', Courier, monospace;
    background-color: #f6f8fa;
    color: #d73a49;
    padding: 0.25em 0.4em;
    border-radius: 3px;
    font-size: 0.85em;
}
pre code {
    background-color: transparent;
    color: inherit;
    padding: 0;
    border-radius: 0;
    font-size: inherit;
}
h1 {
    font-size: 2.2em;
    text-align: center;
    border-bottom: 3px solid #6c7086;
    padding-bottom: 0.5em;
    margin-bottom: 1em;
    color: #11111b;
}
h2 {
    font-size: 1.5em;
    color: #313244;
    margin-top: 2.5em;
    border-left: 4px solid #cba6f7;
    padding-left: 0.6em;
}
p {
    text-align: justify;
    margin: 0.8em 0;
}
.source-tag {
    font-size: 0.8em;
    color: #6c7086;
    font-style: italic;
    text-transform: uppercase;
    letter-spacing: 0.05em;
}
a { color: #89b4fa; }
"""


def build_epub(all_articles: list[dict], date_str: str) -> Path:
    """
    Convert the scraped articles list into a structured EPUB 3 file directly.
    Each article becomes a separate chapter in the table of contents.
    """
    from html import escape as _esc

    book = epub.EpubBook()
    book.set_identifier(f"daily-news-{date_str}")
    book.set_title(f"Daily News — {datetime.strptime(date_str[:8], '%Y%m%d').strftime('%d %B %Y')}")
    book.set_language("en")
    book.add_author("AI News Station")
    book.add_metadata("DC", "description", "Automated daily news digest")

    # Shared CSS item
    css_item = epub.EpubItem(
        uid="main-style",
        file_name="style/main.css",
        media_type="text/css",
        content=EPUB_CSS,
    )
    book.add_item(css_item)

    # Group articles by source, maintaining original chronological order within each source
    from collections import defaultdict
    articles_by_source = defaultdict(list)
    source_order = []
    for article in all_articles:
        source = article.get("source", "Unknown Source").strip()
        if source not in source_order:
            source_order.append(source)
        articles_by_source[source].append(article)

    chapters: list[epub.EpubHtml] = []
    spine: list = ["nav"]
    toc = []

    chapter_index = 0
    for source in source_order:
        source_articles = articles_by_source[source]
        source_chapters = []

        for article in source_articles:
            chapter_title = article.get("title", f"Article {chapter_index + 1}").strip()
            chapter_body = article.get("content", "").strip()

            if not chapter_body:
                chapter_body = article.get("summary", "(No content available)")

            body_html = format_text_to_html(chapter_body)

            # Escape everything interpolated into the XHTML — a title or author
            # containing & or < would otherwise produce an invalid chapter
            safe_title = _esc(chapter_title)
            author_name = article.get("author", "").strip()
            author_suffix = f" · By {_esc(author_name)}" if author_name else ""
            article_url = article.get("url", "")
            url_link = f' · <a href="{_esc(article_url)}">Original Article</a>' if article_url else ""
            source_tag = f'<div class="source-tag">Source: {_esc(source)}{author_suffix}{url_link}</div>'

            image_html = ""
            for img_url in article.get("images", []):
                image_html += f'<div style="text-align: center; margin: 1.5rem 0;"><img src="{_esc(img_url)}" alt="Article Image" style="max-width: 100%; border-radius: 8px; box-shadow: 0 4px 10px rgba(0,0,0,0.1);"/></div>'

            chapter = epub.EpubHtml(
                title=chapter_title,
                file_name=f"text/chap_{chapter_index:03d}.xhtml",
                lang="en",
            )
            chapter.set_content(
                f'<html>'
                f'<head>'
                f'  <title>{safe_title}</title>'
                f'  <link rel="stylesheet" type="text/css" href="../style/main.css"/>'
                f'</head>'
                f'<body>'
                f'  {source_tag}'
                f'  <h2>{safe_title}</h2>'
                f'  {image_html}'
                f'  {body_html}'
                f'</body>'
                f'</html>'
            )
            chapter.add_item(css_item)
            book.add_item(chapter)
            
            source_chapters.append(chapter)
            chapters.append(chapter)
            spine.append(chapter)
            chapter_index += 1

        toc.append((epub.Section(source), source_chapters))

    # Navigation and spine
    book.toc = toc
    book.add_item(epub.EpubNcx())
    book.add_item(epub.EpubNav())
    book.spine = spine

    output_path = DATA_DIR / f"daily-news-{date_str}.epub"
    epub.write_epub(str(output_path), book)
    log.info("EPUB written → %s (%d chapters)", output_path, len(chapters))
    return output_path


# ═══════════════════════════════════════════════════════════════════
# TEXT-TO-SPEECH (edge-tts)
# ═══════════════════════════════════════════════════════════════════

async def generate_tts(text: str, output_path: Path, voice: str) -> None:
    """
    Convert plain text to an MP3 file using Microsoft Edge TTS neural voices.
    edge-tts is free, requires no API key, and works offline after warm-up.
    """
    if not text.strip():
        log.warning("Skipping TTS — empty text for %s", output_path.name)
        return

    log.info("Generating TTS → %s  (voice: %s, chars: %d)", output_path.name, voice, len(text))
    # edge-tts streams from a network service and occasionally drops mid-way
    # on long scripts — retry with backoff rather than losing the whole track
    last_exc: Exception | None = None
    for attempt in range(1, 4):
        try:
            communicate = edge_tts.Communicate(text, voice)
            await communicate.save(str(output_path))
            log.info("Audio saved → %s", output_path)
            return
        except Exception as exc:
            last_exc = exc
            output_path.unlink(missing_ok=True)  # remove any partial file
            log.warning("TTS attempt %d/3 failed for %s: %s", attempt, output_path.name, exc)
            if attempt < 3:
                await asyncio.sleep(5 * attempt)
    raise RuntimeError(f"TTS failed after 3 attempts for {output_path.name}") from last_exc


# ═══════════════════════════════════════════════════════════════════
# MAIN PIPELINE
# ═══════════════════════════════════════════════════════════════════

async def run_pipeline() -> None:
    load_config()
    date_str = datetime.now().strftime("%Y%m%d-%H%M%S")
    DATA_DIR.mkdir(parents=True, exist_ok=True)

    log.info("══════════════════════════════════════════════════")
    log.info("  Daily News Pipeline  —  %s  [LLM: %s]", date_str, LLM_BACKEND)
    log.info("══════════════════════════════════════════════════")

    all_articles: list[dict] = []
    medium_articles: list[dict] = []

    # ── Phase 1: Ingest ──────────────────────────────────────────
    async with async_playwright() as playwright:
        browser, context = await make_browser_context(playwright)

        try:
            # RSS feeds (synchronous feedparser, no browser needed)
            for feed in RSS_FEEDS:
                rss_articles = scrape_rss(feed)
                all_articles.extend(rss_articles)

            # Medium sources (requires stealth browser for JS rendering)
            for tag in MEDIUM_TAGS:
                tag_articles = await scrape_medium_source(context, tag)
                medium_articles.extend(tag_articles)
            all_articles.extend(medium_articles)

            # Load previously scraped URLs
            scraped_urls = set(load_json(DATA_DIR / "scraped_urls.json", []))

            # Filter out already scraped articles
            original_count = len(all_articles)
            all_articles = [a for a in all_articles if a.get("url") not in scraped_urls]
            log.info("Filtered pool: %d -> %d new articles to process", original_count, len(all_articles))

            if not all_articles:
                log.info("No new articles found since last run. Pipeline completed.")
                return

            log.info("Total articles in pool: %d", len(all_articles))

            # Full-page content extraction — concurrent, but capped so a big
            # article pool can't open hundreds of browser pages at once
            log.info(
                "Extracting article content (max %d pages at a time)…",
                EXTRACT_CONCURRENCY,
            )
            extract_sem = asyncio.Semaphore(EXTRACT_CONCURRENCY)

            async def _extract_limited(article: dict) -> dict:
                async with extract_sem:
                    return await extract_article_content(context, article)

            all_articles = list(
                await asyncio.gather(
                    *[_extract_limited(a) for a in all_articles]
                )
            )

        finally:
            await context.close()
            await browser.close()

        # Filter out paywalled content if enabled
        config = load_json(CONFIG_PATH, {})
        if config.get("skip_paywalled_posts", True):
            paywalled_count = sum(1 for a in all_articles if a.get("is_paywalled", False))
            all_articles = [a for a in all_articles if not a.get("is_paywalled", False)]
            if paywalled_count > 0:
                log.info("Skipped %d paywalled/truncated posts", paywalled_count)

    # ── Phase 2: Curate ──────────────────────────────────────────
    all_articles = curate_audio_highlights(all_articles)

    # ── Phase 3: LLM Processing ───────────────────────────────────
    log.info("Building mega-prompt and calling LLM…")
    prompt = build_prompt(all_articles)
    log.info("Prompt size (chars): %d", len(prompt))
    llm_response = call_llm(prompt)
    log.info("LLM response received. Length: %d characters", len(llm_response))
    if len(llm_response) > 0:
        log.info("Response preview (first 500 chars):\n%s", llm_response[:500])
    else:
        log.warning("LLM response is completely empty!")

    # ── Phase 4: Parse XML blocks ─────────────────────────────────
    short_radio  = extract_xml_block(llm_response, "short_radio")
    long_podcast = extract_xml_block(llm_response, "long_podcast")

    # ── Phase 5: Build EPUB ───────────────────────────────────────
    build_epub(all_articles, date_str)

    # ── Phase 5b: Save article sidecar for later audio regen ─────
    # Use date-only key (YYYYMMDD) so regen can always find it by date prefix
    date_only = date_str[:8]
    sidecar_path = DATA_DIR / f"articles-{date_only}.json"
    save_json(sidecar_path, all_articles)
    log.info("Article sidecar saved → %s", sidecar_path)

    # ── Phase 6: Generate audio tracks ───────────────────────────
    radio_path   = DATA_DIR / f"short-radio-{date_str}.mp3"
    podcast_path = DATA_DIR / f"long-podcast-{date_str}.mp3"

    await asyncio.gather(
        generate_tts(short_radio,  radio_path,   VOICE_SHORT),
        generate_tts(long_podcast, podcast_path, VOICE_LONG),
    )

    # Save newly scraped URLs to persistent file.
    # The registry is an ordered list (oldest first) so that truncation drops
    # the OLDEST entries — a set would make the kept 2000 arbitrary and could
    # re-scrape old articles.
    new_urls = [a["url"] for a in all_articles if a.get("url")]
    if new_urls:
        scraped_urls_path = DATA_DIR / "scraped_urls.json"
        scraped_list = load_json(scraped_urls_path, [])
        seen = set(scraped_list)
        for u in new_urls:
            if u not in seen:
                scraped_list.append(u)
                seen.add(u)
        if len(scraped_list) > 2000:
            scraped_list = scraped_list[-2000:]
        save_json(scraped_urls_path, scraped_list)
        log.info("Saved %d total scraped URLs to registry", len(scraped_list))

    # ── Phase 7: Update source activity tracking ─────────────────
    if all_articles:
        activity_path = DATA_DIR / "source_activity.json"
        activity = load_json(activity_path, {})
        # UTC with offset — the frontend computes "days ago" in the browser's
        # locale, so a naive container-local timestamp would skew the result
        iso_now = datetime.now(timezone.utc).isoformat()
        sources_seen = {a["source"] for a in all_articles if a.get("source")}
        for src in sources_seen:
            activity[src] = {
                "last_seen": iso_now,
                "degraded":  src in NO_BROWSER_FETCH_SOURCES,
            }
        save_json(activity_path, activity)
        log.info("Updated source activity tracking for %d sources", len(sources_seen))

    log.info("══════════════════════════════════════════════════")
    log.info("  Pipeline complete. Output files in %s", DATA_DIR)
    log.info("══════════════════════════════════════════════════")


def extract_articles_from_epub(epub_path: Path) -> list[dict]:
    """Fallback parser to reconstruct the articles list from a previously generated EPUB."""
    import ebooklib
    from ebooklib import epub
    from bs4 import BeautifulSoup

    book = epub.read_epub(str(epub_path))
    articles = []
    
    for item in book.get_items():
        if item.get_type() == ebooklib.ITEM_DOCUMENT and item.file_name.startswith("text/chap_"):
            soup = BeautifulSoup(item.get_content(), 'html.parser')
            
            title_tag = soup.find('h2')
            title = title_tag.text.strip() if title_tag else "Unknown Title"
            
            source_tag = soup.find('div', class_='source-tag')
            source = "Unknown Source"
            url = ""
            if source_tag:
                source_text = source_tag.text
                if source_text.startswith("Source: "):
                    source = source_text[8:].split(" · By ")[0].split(" · ")[0]
                url_a = source_tag.find('a')
                if url_a:
                    url = url_a.get('href', '')
                    
            content_pieces = []
            for p in soup.find_all(['p', 'li']):
                content_pieces.append(p.text.strip())
            content = "\n\n".join(content_pieces)
            
            articles.append({
                "title": title,
                "source": source.strip(),
                "url": url,
                "content": content,
            })
            
    return articles


async def run_regen_audio(date_str: str) -> None:
    """
    Audio-only regeneration: loads the articles sidecar for *date_str* (YYYYMMDD prefix),
    re-runs LLM + TTS and overwrites the MP3 files for that date.
    No scraping, no dedup changes, no EPUB rebuild.
    """
    load_config()

    # date_str is the YYYYMMDD group key from the media list.
    # Sidecar is saved as articles-YYYYMMDD.json
    date_only = date_str[:8]
    sidecar_path = DATA_DIR / f"articles-{date_only}.json"

    all_articles = load_json(sidecar_path, None)
    if all_articles is None:
        epub_path = DATA_DIR / f"daily-news-{date_str}.epub"
        if not epub_path.exists():
            log.error(
                "No article sidecar OR epub found for date '%s'.", date_str
            )
            raise FileNotFoundError(f"Article sidecar and EPUB not found for: {date_str}")

        log.warning("No sidecar found. Falling back to extracting articles from existing EPUB: %s", epub_path.name)
        all_articles = extract_articles_from_epub(epub_path)

        # Save it back so future regens are faster
        save_json(sidecar_path, all_articles)

    log.info("Loaded %d articles for regen", len(all_articles))

    # Re-apply highlight curation (voice/sources may have changed)
    all_articles = curate_audio_highlights(all_articles)

    log.info("Building prompt and calling LLM…")
    prompt = build_prompt(all_articles)
    llm_response = call_llm(prompt)
    log.info("LLM response received (%d chars)", len(llm_response))

    short_radio  = extract_xml_block(llm_response, "short_radio")
    long_podcast = extract_xml_block(llm_response, "long_podcast")

    # Name the MP3s with the full group key passed by the frontend (usually
    # YYYYMMDD-HHMMSS, matching the EPUB's timestamp). The server groups media
    # by this exact key, so the new audio joins the same playlist entry instead
    # of appearing as a separate date — and overwrites pipeline audio in place.
    radio_path   = DATA_DIR / f"short-radio-{date_str}.mp3"
    podcast_path = DATA_DIR / f"long-podcast-{date_str}.mp3"

    await asyncio.gather(
        generate_tts(short_radio,  radio_path,   VOICE_SHORT),
        generate_tts(long_podcast, podcast_path, VOICE_LONG),
    )

    log.info("══════════════════════════════════════════════════")
    log.info("  Audio regen complete — %s (files: %s, %s)",
             date_str, radio_path.name, podcast_path.name)
    log.info("══════════════════════════════════════════════════")


if __name__ == "__main__":
    regen_date = os.getenv("REGEN_DATE", "").strip()
    if regen_date:
        asyncio.run(run_regen_audio(regen_date))
    else:
        asyncio.run(run_pipeline())
