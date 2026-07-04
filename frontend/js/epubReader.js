// ── epub.js reader: loading, theming, chapter navigation ────────
import { $, toast, formatDate } from './utils.js';
import { updateSendButton } from './crosspoint.js';

let epubBook      = null; // epub.js Book instance
let epubRendition = null;
let currentChapterIndex = 0;
let totalChapters = 0;

const epubViewer       = $('epub-viewer');
const epubSubtitle     = $('epubSubtitle');
const epubPageInfo     = $('epubPageInfo');
const epubDownloadBtn  = $('epubDownloadBtn');
const prevChapterBtn   = $('prevChapter');
const nextChapterBtn   = $('nextChapter');
const chapterIndicator = $('chapterIndicator');

export async function loadEpub(filename, dateStr) {
  // Tear down previous book
  if (epubBook) {
    try { await epubBook.destroy(); } catch (_) {}
    epubBook = null;
    epubRendition = null;
  }
  prevChapterBtn.disabled = true;
  nextChapterBtn.disabled = true;
  chapterIndicator.textContent = '—';
  epubPageInfo.textContent = '—';

  if (!filename) {
    epubViewer.innerHTML = `
      <div class="epub-empty">
        <div class="big-icon">📄</div>
        <p>EPUB not available for ${formatDate(dateStr)}</p>
        <p style="font-size:0.75rem;color:var(--ctp-overlay0)">Run the scraper to generate the book</p>
      </div>`;
    epubSubtitle.textContent = 'No book for this edition';
    epubDownloadBtn.style.display = 'none';
    $('sendToX4Btn').style.display = 'none';
    return;
  }

  const epubUrl = `/media/${encodeURIComponent(filename)}`;

  epubSubtitle.textContent = `Loading ${formatDate(dateStr)} edition…`;
  epubViewer.innerHTML = `<div class="epub-empty"><div class="big-icon skeleton" style="width:80px;height:80px;border-radius:50%;background:var(--ctp-surface0)"></div><p style="margin-top:1rem">Loading book…</p></div>`;

  try {
    epubBook = ePub(epubUrl);
    epubViewer.innerHTML = ''; // Clear placeholder before rendering

    epubRendition = epubBook.renderTo('epub-viewer', {
      width: '100%',
      height: 640,
      flow: 'scrolled-doc',
      manager: 'default',
    });

    await epubRendition.display();

    // Apply Catppuccin-matched styles inside the iframe
    injectEpubStyles();

    // Load spine for chapter navigation
    await epubBook.ready;
    const spine = epubBook.spine;
    totalChapters = spine.length;
    currentChapterIndex = 0;

    updateChapterUI();
    prevChapterBtn.disabled = false;
    nextChapterBtn.disabled = totalChapters <= 1;

    epubSubtitle.textContent = `${formatDate(dateStr)} · ${totalChapters} chapters`;
    epubPageInfo.textContent = `Chapter 1 of ${totalChapters}`;

    // Download link
    epubDownloadBtn.href = epubUrl;
    epubDownloadBtn.download = filename;
    epubDownloadBtn.style.display = 'inline-flex';
    updateSendButton();

    // Hook into rendition relocations to track chapter
    epubRendition.on('relocated', location => {
      const spineItem = location.start && location.start.index;
      if (typeof spineItem === 'number') {
        currentChapterIndex = spineItem;
        updateChapterUI();
        epubPageInfo.textContent = `Chapter ${currentChapterIndex + 1} of ${totalChapters}`;
      }
    });

    toast(`Loaded ${formatDate(dateStr)} edition`, 'success');

  } catch (err) {
    console.error('EPUB load error:', err);
    epubViewer.innerHTML = `
      <div class="epub-empty">
        <div class="big-icon">⚠️</div>
        <p>Failed to load EPUB</p>
        <p style="font-size:0.75rem;color:var(--ctp-overlay0)">${err.message}</p>
      </div>`;
    epubSubtitle.textContent = 'Error loading book';
    toast(`EPUB error: ${err.message}`, 'error');
  }
}

// ── Apply CSS into the epub.js iframe ────────────────────────────
function injectEpubStyles() {
  if (!epubRendition) return;
  const theme = document.documentElement.getAttribute('data-theme') || 'mocha';
  const isDark = ['mocha','macchiato','frappe'].includes(theme);

  epubRendition.themes.register('ctp', {
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
  epubRendition.themes.select('ctp');
}

// ── Chapter navigation helpers ───────────────────────────────────
function updateChapterUI() {
  prevChapterBtn.disabled = currentChapterIndex <= 0;
  nextChapterBtn.disabled = currentChapterIndex >= totalChapters - 1;
  chapterIndicator.textContent = `${currentChapterIndex + 1} / ${totalChapters}`;
}

prevChapterBtn.addEventListener('click', async () => {
  if (!epubRendition || currentChapterIndex <= 0) return;
  await epubRendition.prev();
  currentChapterIndex = Math.max(0, currentChapterIndex - 1);
  updateChapterUI();
  epubPageInfo.textContent = `Chapter ${currentChapterIndex + 1} of ${totalChapters}`;
});

nextChapterBtn.addEventListener('click', async () => {
  if (!epubRendition || currentChapterIndex >= totalChapters - 1) return;
  await epubRendition.next();
  currentChapterIndex = Math.min(totalChapters - 1, currentChapterIndex + 1);
  updateChapterUI();
  epubPageInfo.textContent = `Chapter ${currentChapterIndex + 1} of ${totalChapters}`;
});

// Re-inject EPUB styles when theme changes
$('themeSelect').addEventListener('change', () => {
  setTimeout(injectEpubStyles, 50); // wait for CSS vars to propagate
});

// ── Keyboard navigation for EPUB ─────────────────────────────────
document.addEventListener('keydown', e => {
  if (!epubRendition) return;
  if (e.target.tagName === 'INPUT' || e.target.tagName === 'SELECT') return;
  if (e.key === 'ArrowLeft')  prevChapterBtn.click();
  if (e.key === 'ArrowRight') nextChapterBtn.click();
});
