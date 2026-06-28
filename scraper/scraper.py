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
from datetime import datetime
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
# Add/remove RSS feeds here; each entry is scraped for TOP_N articles.
RSS_FEEDS = [
    {"name": "BBC News",  "url": "http://feeds.bbci.co.uk/news/rss.xml"},
    # Extend with more feeds as needed:
    # {"name": "Reuters",   "url": "https://feeds.reuters.com/reuters/topNews"},
    # {"name": "Hacker News", "url": "https://hnrss.org/frontpage"},
]

# Medium tags to scrape for top articles
MEDIUM_TAGS = ["terraform"]

# ── Tuning ───────────────────────────────────────────────────────
TOP_N                = 10     # Max articles per source
SIMILARITY_THRESHOLD = 0.6    # TF-IDF cosine sim → "High-Impact Highlight"
TOP_MEDIUM_AUDIO     = 3      # Top Medium posts forced into audio track

# ── Browser fingerprint ──────────────────────────────────────────
USER_AGENT = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
    "AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/125.0.0.0 Safari/537.36"
)

# ── TTS voices ───────────────────────────────────────────────────
VOICE_SHORT = "en-GB-SoniaNeural"   # Flash briefing — crisp British female
VOICE_LONG  = "en-US-GuyNeural"     # Long podcast — warm American male

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
    parsed = feedparser.parse(feed["url"])

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
            "audio_highlight": False,
        })

    log.info("  ↳ %d articles from %s", len(articles), feed["name"])
    return articles


# ═══════════════════════════════════════════════════════════════════
# MEDIUM TAG INGESTION
# ═══════════════════════════════════════════════════════════════════

async def scrape_medium_tag(context, tag: str) -> list[dict]:
    """
    Navigate to a Medium tag page using the stealth browser,
    parse article cards, and return the top TOP_N articles.
    Medium's DOM changes regularly; we use broad link-pattern matching.
    """
    url = f"https://medium.com/tag/{tag}"
    log.info("Scraping Medium tag: %s → %s", tag, url)
    html = await fetch_rendered_html(context, url)

    if not html:
        log.warning("Got empty HTML for Medium tag: %s", tag)
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
            full_url = "https://medium.com" + href.split("?")[0]
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
            # Fall back to link text if no heading found inside
            raw_text = a_tag.get_text(strip=True)
            title = raw_text[:120] if len(raw_text) > 10 else ""

        if not title or len(title) < 10:
            continue

        articles.append({
            "source":          f"Medium/{tag}",
            "title":           title,
            "url":             full_url,
            "summary":         "",
            "content":         "",
            "audio_highlight": False,
        })

        if len(articles) >= TOP_N:
            break

    log.info("  ↳ %d articles from Medium/%s", len(articles), tag)
    return articles


# ═══════════════════════════════════════════════════════════════════
# CONTENT EXTRACTION (Reader Mode via trafilatura)
# ═══════════════════════════════════════════════════════════════════

async def extract_article_content(context, article: dict) -> dict:
    """
    Render the article URL with the stealth browser and pass the full HTML
    to trafilatura's Reader-Mode extractor to get clean, junk-free text.
    Falls back to the RSS summary if extraction fails.
    """
    url = article["url"]
    if not url:
        article["content"] = article.get("summary", "")
        return article

    html = await fetch_rendered_html(context, url)

    if html:
        # trafilatura v2 dropped `no_fallback` and `favor_precision`; support both versions
        try:
            extracted = trafilatura.extract(
                html,
                include_comments=False,
                include_tables=True,
                no_fallback=False,
                favor_precision=True,
            )
        except TypeError:
            # v2.x API — fewer kwargs accepted
            extracted = trafilatura.extract(
                html,
                include_comments=False,
                include_tables=True,
            )
        article["content"] = extracted or article.get("summary", f"[Content unavailable: {url}]")
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

    total_highlights = sum(1 for a in all_articles if a["audio_highlight"])
    log.info("Audio highlights selected: %d / %d", total_highlights, len(all_articles))
    return all_articles


# ═══════════════════════════════════════════════════════════════════
# MEGA-PROMPT CONSTRUCTION
# ═══════════════════════════════════════════════════════════════════

SYSTEM_PROMPT = """\
You are an advanced news editing and broadcasting engine. You are provided with a complete daily pool of articles, along with a marked list of curated audio highlights. Process this text and wrap your outputs in these designated XML tags:

<epub_content>: You must include EVERY SINGLE ONE of the top 10 articles provided from each source. Clean up the raw text to remove web junk, and format them beautifully using Markdown (`##` for article titles as chapters). This must serve as an unabridged daily newspaper.

<short_radio>: Focus ONLY on the marked audio highlights and top stories. Act as a punchy, concise radio news anchor delivering a brief flash briefing similar to an Android Auto or smart alarm clock setup.
  CRITICAL TIME CONSTRAINT: This script MUST be crisp and tightly edited. It must absolutely NOT exceed a 30-minute reading time under any circumstances. Target a high-density delivery between 500 to 2,000 words total.
  Structure: Continuous text script. For each story, use: "[Source] reported that [Headline], [details]". NO markdown formatting, asterisks, or bolding. Translate code/terminal commands into conceptual, easy-to-understand audio explanations.

<long_podcast>: Focus on the marked audio highlights and top stories, expanding on their context. Act as a casual, conversational tech podcast host delivering a seamless monologue. Use smooth verbal transitions. NO speaker tags or audio cues. Pure text prose optimized for TTS.\
"""


def build_prompt(all_articles: list[dict]) -> str:
    """Assemble the single mega-prompt with the full article pool."""
    highlights = [a for a in all_articles if a.get("audio_highlight")]

    highlight_summary = "\n".join(
        f"  • [{a['source']}] {a['title']}" for a in highlights
    ) or "  (none flagged)"

    pool_sections = []
    for i, a in enumerate(all_articles, 1):
        flag = " ★ [AUDIO HIGHLIGHT]" if a.get("audio_highlight") else ""
        pool_sections.append(
            f"=== ARTICLE {i}{flag} ===\n"
            f"Source:  {a['source']}\n"
            f"Title:   {a['title']}\n"
            f"URL:     {a['url']}\n\n"
            f"{a.get('content') or a.get('summary', '(no content)')}\n"
        )

    return "\n\n".join([
        SYSTEM_PROMPT,
        f"── CURATED AUDIO HIGHLIGHTS ({len(highlights)} articles) ──",
        highlight_summary,
        f"── FULL DAILY POOL ({len(all_articles)} articles) ──",
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


def build_epub(content_md: str, date_str: str) -> Path:
    """
    Convert the LLM-generated Markdown into a structured EPUB 3 file.
    Each ## heading becomes a separate chapter in the table of contents.
    """
    book = epub.EpubBook()
    book.set_identifier(f"daily-news-{date_str}")
    book.set_title(f"Daily News — {datetime.strptime(date_str, '%Y%m%d').strftime('%d %B %Y')}")
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

    chapters: list[epub.EpubHtml] = []
    spine: list = ["nav"]

    # Split content by ## headings — each becomes a chapter
    raw_sections = re.split(r"(?m)^## ", content_md)
    # Discard any preamble before the first ##
    sections = [s for s in raw_sections if s.strip()]

    # Fallback: if no chapters were parsed, treat the entire string as a single chapter
    if not sections:
        sections = [f"Daily News Digest\n\n{content_md or 'No content generated.'}"]

    for idx, section in enumerate(sections):
        lines = section.strip().split("\n", 1)
        chapter_title = lines[0].strip() if lines else f"Article {idx + 1}"
        chapter_body  = lines[1].strip() if len(lines) > 1 else ""

        # Lightweight Markdown → HTML conversion
        paragraphs = [p.strip() for p in chapter_body.split("\n\n") if p.strip()]
        html_paras = []
        for para in paragraphs:
            # Bold and italic inline markup
            para = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", para)
            para = re.sub(r"\*(.+?)\*",     r"<em>\1</em>", para)
            # Inline code
            para = re.sub(r"`(.+?)`", r"<code>\1</code>", para)
            html_paras.append(f"<p>{para}</p>")

        body_html = "\n".join(html_paras) or "<p>(content unavailable)</p>"

        chapter = epub.EpubHtml(
            title=chapter_title,
            file_name=f"text/chap_{idx:03d}.xhtml",
            lang="en",
        )
        chapter.set_content(
            f'<html>'
            f'<head>'
            f'  <title>{chapter_title}</title>'
            f'  <link rel="stylesheet" type="text/css" href="../style/main.css"/>'
            f'</head>'
            f'<body>'
            f'  <h2>{chapter_title}</h2>'
            f'  {body_html}'
            f'</body>'
            f'</html>'
        )
        chapter.add_item(css_item)
        book.add_item(chapter)
        chapters.append(chapter)
        spine.append(chapter)

    # Navigation and spine
    book.toc = [(epub.Section("Daily Articles"), chapters)]
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
    communicate = edge_tts.Communicate(text, voice)
    await communicate.save(str(output_path))
    log.info("Audio saved → %s", output_path)


# ═══════════════════════════════════════════════════════════════════
# MAIN PIPELINE
# ═══════════════════════════════════════════════════════════════════

async def run_pipeline() -> None:
    date_str = datetime.now().strftime("%Y%m%d")
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

            # Medium tags (requires stealth browser for JS rendering)
            for tag in MEDIUM_TAGS:
                tag_articles = await scrape_medium_tag(context, tag)
                medium_articles.extend(tag_articles)
            all_articles.extend(medium_articles)

            log.info("Total articles in pool: %d", len(all_articles))

            # Full-page content extraction — run concurrently
            log.info("Extracting article content (may take a few minutes)…")
            all_articles = list(
                await asyncio.gather(
                    *[extract_article_content(context, a) for a in all_articles]
                )
            )

        finally:
            await context.close()
            await browser.close()

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
    epub_content = extract_xml_block(llm_response, "epub_content")
    short_radio  = extract_xml_block(llm_response, "short_radio")
    long_podcast = extract_xml_block(llm_response, "long_podcast")

    # Fallback: use raw response for EPUB if XML tags are missing
    if not epub_content:
        log.warning("Falling back to raw LLM response for EPUB content")
        epub_content = llm_response

    # ── Phase 5: Build EPUB ───────────────────────────────────────
    if epub_content:
        build_epub(epub_content, date_str)

    # ── Phase 6: Generate audio tracks ───────────────────────────
    radio_path   = DATA_DIR / f"short-radio-{date_str}.mp3"
    podcast_path = DATA_DIR / f"long-podcast-{date_str}.mp3"

    await asyncio.gather(
        generate_tts(short_radio,  radio_path,   VOICE_SHORT),
        generate_tts(long_podcast, podcast_path, VOICE_LONG),
    )

    log.info("══════════════════════════════════════════════════")
    log.info("  Pipeline complete. Output files in %s", DATA_DIR)
    log.info("══════════════════════════════════════════════════")


if __name__ == "__main__":
    asyncio.run(run_pipeline())
