// ── epub.js readers: main edition + TLDR digest ─────────────────
// Two independent reader instances built from one factory: the full
// daily-news EPUB on top, and the companion TLDR digest below it.
import { $, toast, formatDate } from './utils.js';

// ── Shared: Catppuccin theme injected into the epub.js iframe ────
function injectEpubStyles(rendition) {
  if (!rendition) return;
  const theme = document.documentElement.getAttribute('data-theme') || 'mocha';
  const isDark = ['mocha', 'macchiato', 'frappe'].includes(theme);

  rendition.themes.register('ctp', {
    body: {
      'font-family': "'Georgia', serif",
      'line-height': '1.8',
      'padding': '1.5em 2em',
      'color':      isDark ? '#cdd6f4' : '#4c4f69',
      'background': isDark ? '#1e1e2e' : '#eff1f5',
      'font-size':  '16px',
    },
    h1: { 'color': isDark ? '#cba6f7' : '#8839ef', 'border-bottom': '2px solid currentColor' },
    h2: { 'color': isDark ? '#89b4fa' : '#1e66f5', 'margin-top': '2em' },
    a:  { 'color': isDark ? '#74c7ec' : '#209fb5' },
    p:  { 'text-align': 'justify', 'margin': '0.75em 0' },
    pre: {
      'background-color': isDark ? '#11111b' : '#f6f8fa',
      'color': isDark ? '#cdd6f4' : '#24292e',
      'padding': '1em',
      'border-radius': '6px',
      'border': isDark ? '1px solid #45475a' : '1px solid #e1e4e8',
      'overflow-x': 'auto',
      'font-family': "'Courier New', Courier, monospace",
      'font-size': '0.85em',
      'line-height': '1.45',
      'margin': '1.5em 0'
    },
    code: {
      'font-family': "'Courier New', Courier, monospace",
      'background-color': isDark ? '#11111b' : '#f6f8fa',
      'color': isDark ? '#f38ba8' : '#d73a49',
      'padding': '0.2em 0.4em',
      'border-radius': '3px',
      'font-size': '0.85em'
    },
    'pre code': {
      'background-color': 'transparent',
      'color': 'inherit',
      'padding': '0',
      'border-radius': '0',
      'font-size': 'inherit'
    }
  });
  rendition.themes.select('ctp');
}

// ── Reader factory ───────────────────────────────────────────────
function createReader(cfg) {
  const viewer       = $(cfg.viewerId);
  const subtitle     = $(cfg.subtitleId);
  const pageInfo     = $(cfg.pageInfoId);
  const downloadBtn  = $(cfg.downloadId);
  const prevBtn      = $(cfg.prevId);
  const nextBtn      = $(cfg.nextId);
  const indicator    = $(cfg.indicatorId);

  let book = null;
  let rendition = null;
  let currentChapterIndex = 0;
  let totalChapters = 0;

  function updateChapterUI() {
    prevBtn.disabled = currentChapterIndex <= 0;
    nextBtn.disabled = currentChapterIndex >= totalChapters - 1;
    indicator.textContent = `${currentChapterIndex + 1} / ${totalChapters}`;
  }

  async function load(filename, dateStr) {
    // Tear down previous book
    if (book) {
      try { await book.destroy(); } catch (_) {}
      book = null;
      rendition = null;
    }
    prevBtn.disabled = true;
    nextBtn.disabled = true;
    indicator.textContent = '—';
    pageInfo.textContent = '—';

    if (!filename) {
      viewer.innerHTML = `
        <div class="epub-empty">
          <div class="big-icon">${cfg.emptyIcon}</div>
          <p>${cfg.label} not available for ${formatDate(dateStr)}</p>
          <p style="font-size:0.75rem;color:var(--ctp-overlay0)">${cfg.emptyHint}</p>
        </div>`;
      subtitle.textContent = `No ${cfg.label.toLowerCase()} for this edition`;
      downloadBtn.style.display = 'none';
      return;
    }

    const epubUrl = `/media/${encodeURIComponent(filename)}`;

    subtitle.textContent = `Loading ${formatDate(dateStr)} edition…`;
    viewer.innerHTML = `<div class="epub-empty"><div class="big-icon skeleton" style="width:80px;height:80px;border-radius:50%;background:var(--ctp-surface0)"></div><p style="margin-top:1rem">Loading book…</p></div>`;

    try {
      book = ePub(epubUrl);
      viewer.innerHTML = ''; // Clear placeholder before rendering

      rendition = book.renderTo(cfg.viewerId, {
        width: '100%',
        height: cfg.height,
        flow: 'scrolled-doc',
        manager: 'default',
      });

      await rendition.display();
      injectEpubStyles(rendition);

      // Load spine for chapter navigation
      await book.ready;
      totalChapters = book.spine.length;
      currentChapterIndex = 0;

      updateChapterUI();
      prevBtn.disabled = false;
      nextBtn.disabled = totalChapters <= 1;

      subtitle.textContent = `${formatDate(dateStr)} · ${totalChapters} chapters`;
      pageInfo.textContent = `Chapter 1 of ${totalChapters}`;

      // Download link
      downloadBtn.href = epubUrl;
      downloadBtn.download = filename;
      downloadBtn.style.display = 'inline-flex';

      // Hook into rendition relocations to track chapter
      rendition.on('relocated', location => {
        const spineItem = location.start && location.start.index;
        if (typeof spineItem === 'number') {
          currentChapterIndex = spineItem;
          updateChapterUI();
          pageInfo.textContent = `Chapter ${currentChapterIndex + 1} of ${totalChapters}`;
        }
      });

      if (cfg.isMain) toast(`Loaded ${formatDate(dateStr)} edition`, 'success');

    } catch (err) {
      console.error('EPUB load error:', err);
      viewer.innerHTML = `
        <div class="epub-empty">
          <div class="big-icon">⚠️</div>
          <p>Failed to load EPUB</p>
          <p style="font-size:0.75rem;color:var(--ctp-overlay0)">${err.message}</p>
        </div>`;
      subtitle.textContent = 'Error loading book';
      if (cfg.isMain) toast(`EPUB error: ${err.message}`, 'error');
    }
  }

  prevBtn.addEventListener('click', async () => {
    if (!rendition || currentChapterIndex <= 0) return;
    await rendition.prev();
    currentChapterIndex = Math.max(0, currentChapterIndex - 1);
    updateChapterUI();
    pageInfo.textContent = `Chapter ${currentChapterIndex + 1} of ${totalChapters}`;
  });

  nextBtn.addEventListener('click', async () => {
    if (!rendition || currentChapterIndex >= totalChapters - 1) return;
    await rendition.next();
    currentChapterIndex = Math.min(totalChapters - 1, currentChapterIndex + 1);
    updateChapterUI();
    pageInfo.textContent = `Chapter ${currentChapterIndex + 1} of ${totalChapters}`;
  });

  return {
    load,
    reinjectStyles: () => injectEpubStyles(rendition),
    hasRendition: () => !!rendition,
    prevBtn,
    nextBtn,
  };
}

// ── Instances ────────────────────────────────────────────────────
const mainReader = createReader({
  viewerId:   'epub-viewer',
  subtitleId: 'epubSubtitle',
  pageInfoId: 'epubPageInfo',
  downloadId: 'epubDownloadBtn',
  prevId:     'prevChapter',
  nextId:     'nextChapter',
  indicatorId: 'chapterIndicator',
  height:     640,
  emptyIcon:  '📄',
  label:      'EPUB',
  emptyHint:  'Run the scraper to generate the book',
  isMain:     true,
});

const tldrReader = createReader({
  viewerId:   'tldr-viewer',
  subtitleId: 'tldrSubtitle',
  pageInfoId: 'tldrPageInfo',
  downloadId: 'tldrDownloadBtn',
  prevId:     'tldrPrevChapter',
  nextId:     'tldrNextChapter',
  indicatorId: 'tldrChapterIndicator',
  height:     480,
  emptyIcon:  '⚡',
  label:      'TLDR digest',
  emptyHint:  'Generated alongside new editions from now on',
  isMain:     false,
});

export async function loadEpub(filename, dateStr) {
  return mainReader.load(filename, dateStr);
}

export async function loadTldrEpub(filename, dateStr) {
  return tldrReader.load(filename, dateStr);
}

// Re-inject EPUB styles when theme changes
$('themeSelect').addEventListener('change', () => {
  setTimeout(() => {
    mainReader.reinjectStyles();
    tldrReader.reinjectStyles();
  }, 50); // wait for CSS vars to propagate
});

// ── Keyboard navigation (main reader only, as before) ───────────
document.addEventListener('keydown', e => {
  if (!mainReader.hasRendition()) return;
  if (e.target.tagName === 'INPUT' || e.target.tagName === 'SELECT') return;
  if (e.key === 'ArrowLeft')  mainReader.prevBtn.click();
  if (e.key === 'ArrowRight') mainReader.nextBtn.click();
});
