"""Broadsheet Newspaper Edition Generator.

Renders an A3 landscape multi-column print-density PDF newspaper spread
from curated daily articles, with QR codes linking back to online sources.
"""

import base64
import html
import logging
import os
import re
from datetime import datetime
from pathlib import Path

log = logging.getLogger(__name__)

# Default data directory matching station conventions
DATA_DIR = Path(os.getenv("DATA_DIR", "/app/data"))
TEMPLATE_PATH = Path(__file__).parent / "templates" / "broadsheet.html"


def is_broadsheet_enabled(cfg: dict | None = None) -> bool:
    """
    Check if the broadsheet edition is enabled.

    Precedence:
      1. Runtime environment variable: ENABLE_BROADSHEET_EDITION (true/1/yes vs false/0/no)
      2. Configuration file: editions.broadsheet.enabled
      3. Configuration file: enable_broadsheet_edition
      4. Default: False
    """
    env_val = os.getenv("ENABLE_BROADSHEET_EDITION", "").strip().lower()
    if env_val in ("true", "1", "yes"):
        return True
    if env_val in ("false", "0", "no"):
        return False

    if cfg is None:
        try:
            from scraper import CONFIG_PATH, load_json
            cfg = load_json(CONFIG_PATH, {})
        except Exception:
            cfg = {}

    if isinstance(cfg, dict):
        editions = cfg.get("editions")
        if isinstance(editions, dict):
            broadsheet = editions.get("broadsheet")
            if isinstance(broadsheet, dict) and "enabled" in broadsheet:
                return bool(broadsheet["enabled"])

        if "enable_broadsheet_edition" in cfg:
            return bool(cfg["enable_broadsheet_edition"])

    return False


def make_qr_data_uri(url: str) -> str | None:
    """Generate an 8-bit grayscale PNG QR code as a base64 Data URI."""
    if not url:
        return None
    try:
        from scraper import make_qr_png
        png_bytes = make_qr_png(url)
    except Exception:
        try:
            import struct, zlib, segno
            matrix = segno.make(url, error="l").matrix
            size = len(matrix)
            scale = 8
            border = 4
            w = h = (size + 2 * border) * scale

            raw = bytearray()
            for row in range(h):
                raw.append(0)  # PNG filter type: None
                my = row // scale - border
                for col in range(w):
                    mx = col // scale - border
                    dark = (0 <= my < size and 0 <= mx < size and matrix[my][mx])
                    raw.append(0 if dark else 255)

            def chunk(tag: bytes, data: bytes) -> bytes:
                body = tag + data
                return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

            png_bytes = (
                b"\x89PNG\r\n\x1a\n"
                + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 0, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(bytes(raw), 6))
                + chunk(b"IEND", b"")
            )
        except Exception as exc:
            log.warning("Broadsheet QR code generation failed for %s: %s", url, exc)
            return None

    if png_bytes:
        b64 = base64.b64encode(png_bytes).decode("ascii")
        return f"data:image/png;base64,{b64}"
    return None


def format_display_date(date_str: str) -> str:
    """Format YYYYMMDD[-HHMMSS] to a print newspaper date like 'Monday, September 7, 2026'."""
    m = re.match(r"^(\d{4})(\d{2})(\d{2})", date_str)
    if m:
        try:
            dt = datetime(int(m.group(1)), int(m.group(2)), int(m.group(3)))
            return dt.strftime("%A, %B %-d, %Y")
        except Exception:
            pass
    return date_str


def _render_article_html(article: dict, is_lead: bool = False, is_secondary: bool = False) -> str:
    """Render a single article HTML block for the multi-column spread."""
    title = html.escape(article.get("title", "Untitled Story"))
    source = html.escape(article.get("source", "News").upper())
    url = article.get("url", "")
    author = article.get("author") or article.get("byline") or article.get("source", "Staff")
    byline_str = html.escape(f"By {author}") if not author.startswith("By ") else html.escape(author)

    qr_uri = make_qr_data_uri(url) if url else None
    qr_html = ""
    if qr_uri:
        qr_html = (
            f'<div class="article-qr-wrap">'
            f'<img src="{qr_uri}" alt="QR code" />'
            f'<span class="qr-caption">Read Online</span>'
            f'</div>'
        )

    # Headline style
    if is_lead:
        headline_cls = "banner"
        article_cls = "article lead-story"
    elif is_secondary:
        headline_cls = "primary"
        article_cls = "article"
    else:
        headline_cls = "secondary"
        article_cls = "article"

    # Extract body paragraphs
    content = article.get("text") or article.get("summary") or article.get("content") or ""
    paragraphs = [p.strip() for p in content.split("\n") if p.strip()]
    if not paragraphs:
        paragraphs = ["Full report available via the digital edition."]

    paras_html = "".join(f"<p>{html.escape(p)}</p>" for p in paragraphs)

    return f"""
    <article class="{article_cls}">
      <div class="kicker">&#9670; {source}</div>
      <h2 class="headline {headline_cls}">{title}</h2>
      <div class="byline">
        <span class="source-tag">{source}</span>
        <span>{byline_str}</span>
      </div>
      {qr_html}
      <div class="article-body">
        {paras_html}
      </div>
    </article>
    """


def render_spreads_html(all_articles: list[dict], date_str: str) -> str:
    """
    Layout articles across dual-page spreads (Left 3 cols, Spine, Right 3 cols).
    Spread 1 has the prominent newspaper masthead.
    Subsequent spreads have running headers.
    """
    if not all_articles:
        return """
        <div class="spread-container">
          <div class="masthead-wrap">
            <div class="top-meta-bar">
              <span>Weather: Clear &bull; 18°C</span>
              <span>The Independent Digest</span>
              <span>Late City Final</span>
            </div>
            <div class="masthead">
              <div class="masthead-side-slug">NO. 1 IN AUTOMATED REPORTING</div>
              <h1 class="masthead-title">THE DAILY BROADSHEET</h1>
              <div class="masthead-side-slug right">GLOBAL &amp; DOMESTIC ED.</div>
            </div>
            <div class="edition-rule">
              <span>Printed Daily</span>
              <span>All News Verified</span>
              <span>Price: Complimentary</span>
            </div>
          </div>
          <div class="spread-body">
            <div class="page-half left-half">
              <p>No articles available for this edition date.</p>
            </div>
            <div class="spine-gutter"><div class="spine-rule"></div></div>
            <div class="page-half right-half"></div>
          </div>
        </div>
        """

    display_date = format_display_date(date_str)
    spreads = []

    # Distribution: ~6 articles per spread (3 per half)
    articles_per_spread = 6
    chunked_spreads = [
        all_articles[i:i + articles_per_spread]
        for i in range(0, len(all_articles), articles_per_spread)
    ]

    for s_idx, spread_articles in enumerate(chunked_spreads):
        is_front_page = (s_idx == 0)
        page_left_num = (s_idx * 2) + 1
        page_right_num = (s_idx * 2) + 2

        mid = (len(spread_articles) + 1) // 2
        left_articles = spread_articles[:mid]
        right_articles = spread_articles[mid:]

        # Front page header vs interior running header
        if is_front_page:
            header_html = f"""
            <div class="masthead-wrap">
              <div class="top-meta-bar">
                <span>The Independent Digest &bull; Est. 2024</span>
                <span>{display_date}</span>
                <span>Late City Final Edition</span>
              </div>
              <div class="masthead">
                <div class="masthead-side-slug">
                  HIGH DENSITY PRINT<br>
                  CURATED INTELLIGENCE
                </div>
                <h1 class="masthead-title">THE DAILY BROADSHEET</h1>
                <div class="masthead-side-slug right">
                  SIX-COLUMN SPREAD<br>
                  DIGITAL SYNCHRONIZED
                </div>
              </div>
              <div class="edition-rule">
                <span>Vol. CLI No. 58,400</span>
                <span>{display_date}</span>
                <span>Price: Complimentary</span>
              </div>
            </div>
            """
        else:
            header_html = f"""
            <div class="running-header">
              <span>Page {page_left_num} &bull; The Daily Broadsheet</span>
              <span>SECTION {s_idx + 1} &bull; CONTINUED DISPATCHES</span>
              <span>{display_date} &bull; Page {page_right_num}</span>
            </div>
            """

        # Left half articles
        left_parts = []
        if is_front_page:
            # Add an editorial index box in column 1 of page 1
            sources = sorted(list({a.get("source", "General") for a in all_articles}))
            source_bullets = " &bull; ".join(html.escape(s) for s in sources[:6])
            left_parts.append(f"""
            <div class="feature-box">
              <div class="feature-box-title">Today's Sources &amp; Coverage</div>
              <div style="font-size: 8pt; line-height: 1.3; color: var(--ink);">
                Reporting synchronized from {source_bullets}. Scan any story's QR code to view live online dispatches.
              </div>
            </div>
            """)

        for i, art in enumerate(left_articles):
            is_lead = (is_front_page and i == 0)
            left_parts.append(_render_article_html(art, is_lead=is_lead, is_secondary=not is_lead and i == 1))

        # Right half articles
        right_parts = []
        for i, art in enumerate(right_articles):
            right_parts.append(_render_article_html(art, is_lead=False, is_secondary=(i == 0)))

        left_html = "\n".join(left_parts)
        right_html = "\n".join(right_parts)

        spreads.append(f"""
        <div class="spread-container">
          {header_html}
          <div class="spread-body">
            <div class="page-half left-half">
              {left_html}
            </div>
            <div class="spine-gutter">
              <div class="spine-rule"></div>
            </div>
            <div class="page-half right-half">
              {right_html}
            </div>
          </div>
        </div>
        """)

    return "\n".join(spreads)


def render_broadsheet_html(all_articles: list[dict], date_str: str, template_path: Path | None = None) -> str:
    """Render the full standalone broadsheet HTML document."""
    tmpl_path = template_path or TEMPLATE_PATH
    with open(tmpl_path, "r", encoding="utf-8") as f:
        template = f.read()

    display_date = format_display_date(date_str)
    spreads_html = render_spreads_html(all_articles, date_str)

    html_out = template.replace("{{ masthead_title }}", "THE DAILY BROADSHEET")
    html_out = html_out.replace("{{ date_display }}", display_date)
    html_out = html_out.replace("{{ spreads_html }}", spreads_html)

    return html_out


async def build_broadsheet_pdf(
    all_articles: list[dict],
    date_str: str,
    output_path: Path | None = None,
    cfg: dict | None = None,
) -> Path | None:
    """
    Compile the broadsheet newspaper PDF if enabled.
    Returns the created Path or None if broadsheet is disabled or rendering failed.
    """
    if not is_broadsheet_enabled(cfg):
        log.info("Broadsheet edition is disabled — skipping PDF compilation")
        return None

    out_path = output_path or (DATA_DIR / f"daily-broadsheet-{date_str}.pdf")
    log.info("Compiling Broadsheet Newspaper Edition → %s (%d articles)", out_path, len(all_articles))

    html_content = render_broadsheet_html(all_articles, date_str)

    # Save HTML sidecar for debugging / inspection
    html_debug_path = out_path.with_suffix(".html")
    try:
        html_debug_path.write_text(html_content, encoding="utf-8")
    except Exception as exc:
        log.debug("Could not write debug HTML sidecar: %s", exc)

    # 1. Attempt Playwright Chromium rendering (native multi-column CSS + @page support)
    try:
        from playwright.async_api import async_playwright
        async with async_playwright() as pw:
            browser = await pw.chromium.launch(
                args=[
                    "--no-sandbox",
                    "--disable-setuid-sandbox",
                    "--disable-dev-shm-usage",
                    "--font-render-hinting=medium",
                ]
            )
            page = await browser.new_page()
            # 420mm x 297mm in pixels at 96 DPI: 1587 x 1123
            await page.set_viewport_size({"width": 1587, "height": 1123})
            await page.set_content(html_content, wait_until="networkidle")

            # A3 landscape: 420mm width, 297mm height
            await page.pdf(
                path=str(out_path),
                format="A3",
                landscape=True,
                print_background=True,
                margin={"top": "0mm", "bottom": "0mm", "left": "0mm", "right": "0mm"},
            )
            await browser.close()

        log.info("Broadsheet PDF successfully generated via Playwright → %s", out_path)
        return out_path
    except Exception as pw_err:
        log.warning("Playwright rendering failed (%s); trying WeasyPrint fallback", pw_err)

    # 2. Fallback to WeasyPrint if available
    try:
        from weasyprint import HTML
        HTML(string=html_content).write_pdf(str(out_path))
        log.info("Broadsheet PDF successfully generated via WeasyPrint fallback → %s", out_path)
        return out_path
    except Exception as weasy_err:
        log.error("Both Playwright and WeasyPrint failed to render broadsheet PDF: %s", weasy_err)
        return None
