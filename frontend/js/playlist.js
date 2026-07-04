// ── Media library: playlist, date selection, audio players ──────
import { $, toast, setStatus, formatDate, isNewEdition } from './utils.js';
import { loadEpub, loadTldrEpub } from './epubReader.js';
import { refreshSendButtonForDate } from './crosspoint.js';
import { isScrapingActive } from './scraperStatus.js';

export let mediaData  = [];   // raw /api/media response
export let activeDate = null; // currently selected date string (YYYYMMDD)

const audioPlaylist    = $('audioPlaylist');
const playlistSubtitle = $('playlistSubtitle');
const refreshBtn       = $('refreshBtn');

export function updateStatusBar() {
  if (isScrapingActive) return;
  const newCount = mediaData.filter(item => isNewEdition(item.date)).length;
  if (newCount > 0) {
    setStatus(`${newCount} new edition(s) available`, 'active');
  } else {
    setStatus('All editions read · System up to date', '');
  }
}

// ── Fetch media list from Rust server ────────────────────────────
export async function loadMediaList(silent = false) {
  if (!silent) {
    setStatus('Fetching media library…', 'loading');
    refreshBtn.disabled = true;
    audioPlaylist.innerHTML = `
      <div class="skeleton" style="height:44px; border-radius:10px; margin-bottom:0.5rem;"></div>
      <div class="skeleton" style="height:44px; border-radius:10px; margin-bottom:0.5rem;"></div>
      <div class="skeleton" style="height:44px; border-radius:10px;"></div>
    `;
  }

  try {
    const resp = await fetch('/api/media');
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const data = await resp.json();
    mediaData = data.dates || [];

    if (mediaData.length === 0) {
      if (!silent) setStatus('No media found. Run the scraper to generate content.', 'error');
      audioPlaylist.innerHTML = '<span style="color:var(--ctp-overlay1);font-size:0.82rem;padding:0.5rem;">No editions available</span>';
      return;
    }

    renderPlaylist();
    if (!silent) updateStatusBar();

    // Auto-select the most recent date if nothing is selected yet
    if (!activeDate && mediaData.length > 0) {
      selectDate(mediaData[0].date);
    }

  } catch (err) {
    if (!silent) {
      setStatus(`Error loading media: ${err.message}`, 'error');
      audioPlaylist.innerHTML = '<span style="color:var(--ctp-red);font-size:0.82rem">⚠ Could not load library</span>';
    }
    toast(`Failed to load media: ${err.message}`, 'error');
  } finally {
    refreshBtn.disabled = false;
  }
}

// ── Render vertical playlist selection ───────────────────────────
export function renderPlaylist() {
  audioPlaylist.innerHTML = '';
  // Limit history to last 10 runs
  const last10 = mediaData.slice(0, 10);
  last10.forEach(entry => {
    const item = document.createElement('button');
    item.className = `playlist-item${entry.date === activeDate ? ' active' : ''}`;
    item.setAttribute('role', 'option');
    item.setAttribute('aria-selected', entry.date === activeDate);
    item.setAttribute('data-date', entry.date);

    // Build content badges
    let badgesHtml = '';
    if (isNewEdition(entry.date)) {
      badgesHtml += '<span class="badge" style="background:var(--ctp-green); color:var(--ctp-crust); font-weight:600; animation: pulse-dot 1.5s infinite;">✨ New</span>';
    }
    if (entry.epub) badgesHtml += '<span class="badge">📖 Book</span>';
    if (entry.tldr) badgesHtml += '<span class="badge">⚡ TLDR</span>';
    if (entry.radio) badgesHtml += '<span class="badge">📻 Radio</span>';
    if (entry.podcast) badgesHtml += '<span class="badge">🎧 Podcast</span>';

    item.innerHTML = `
      <div class="playlist-item-meta">
        <span class="playlist-item-date">${formatDate(entry.date)}</span>
        <div class="playlist-item-badges">
          ${badgesHtml || '<span class="badge">Processing</span>'}
        </div>
      </div>
      <span style="font-size: 1.1rem; opacity: 0.8;">▶</span>
    `;

    item.addEventListener('click', () => selectDate(entry.date));
    audioPlaylist.appendChild(item);
  });
}

// ── Select a date and render all media for it ────────────────────
export function selectDate(dateStr) {
  activeDate = dateStr;

  // Update item active states
  document.querySelectorAll('.playlist-item').forEach(item => {
    const isActive = item.dataset.date === dateStr;
    item.classList.toggle('active', isActive);
    item.setAttribute('aria-selected', isActive);
  });

  // Mark as clicked/viewed in local storage to clear the New badge
  const clicked = JSON.parse(localStorage.getItem('clickedEditions') || '[]');
  if (!clicked.includes(dateStr)) {
    clicked.push(dateStr);
    localStorage.setItem('clickedEditions', JSON.stringify(clicked));
    // Re-render playlist and update status bar
    renderPlaylist();
    updateStatusBar();
  }

  playlistSubtitle.textContent = `Active: ${formatDate(dateStr)}`;

  const entry = mediaData.find(e => e.date === dateStr);
  if (!entry) return;

  renderAudioPlayer('radio',   entry.radio,   dateStr, entry);
  renderAudioPlayer('podcast', entry.podcast, dateStr, entry);
  loadEpub(entry.epub, dateStr);
  loadTldrEpub(entry.tldr, dateStr);

  // Rebuild buttons need the saved articles sidecar (or EPUB fallback) on
  // the server, so gate both on this edition having a book. TLDR shows
  // even when entry.tldr is missing — that's the backfill path.
  const epubRegenBtn = $('regenBtn-epub');
  if (epubRegenBtn) {
    epubRegenBtn.style.display = entry.epub ? '' : 'none';
    epubRegenBtn.onclick = () => window.triggerRegenAudio(dateStr, 'epub');
  }
  const tldrRegenBtn = $('regenBtn-tldr');
  if (tldrRegenBtn) {
    tldrRegenBtn.style.display = entry.epub ? '' : 'none';
    tldrRegenBtn.onclick = () => window.triggerRegenAudio(dateStr, 'tldr');
  }

  refreshSendButtonForDate();
}

// ── Render an audio player card ──────────────────────────────────
function renderAudioPlayer(type, filename, dateStr, entry) {
  const contentEl = type === 'radio' ? $('radioContent') : $('podcastContent');
  const label     = type === 'radio' ? 'Flash Briefing' : 'Full Podcast';

  if (!filename) {
    // Show regen button if an EPUB exists for this date (meaning sidecar should be present too)
    const hasEpub = entry && entry.epub;
    contentEl.innerHTML = `
      <div class="unavailable-state">
        <div class="icon">🔇</div>
        <p>${label} not available for ${formatDate(dateStr)}</p>
        ${hasEpub ? `
        <button
          id="regenBtn-${type}"
          onclick="triggerRegenAudio('${dateStr}', '${type}')"
          style="margin-top:0.75rem; padding:0.45rem 1rem; font-size:0.78rem; font-weight:600;
                 background:var(--ctp-blue); color:var(--ctp-base); border:none;
                 border-radius:8px; cursor:pointer; display:inline-flex; align-items:center; gap:0.4rem;
                 transition:opacity 0.2s;"
          onmouseover="this.style.opacity='0.8'" onmouseout="this.style.opacity='1'">
          🔁 Re-generate Audio
        </button>
        <p style="font-size:0.7rem;color:var(--ctp-overlay0);margin-top:0.4rem;">
          Re-runs LLM &amp; TTS using today's scraped articles — no full re-scrape needed
        </p>` : `
        <p style="font-size:0.72rem;color:var(--ctp-overlay0)">Run the scraper to generate audio</p>`}
      </div>`;
    return;
  }

  const mediaUrl = `/media/${encodeURIComponent(filename)}`;
  const audioId  = `audio-${type}`;
  const fillId   = `fill-${type}`;

  contentEl.innerHTML = `
    <div class="audio-wrapper">
      <audio id="${audioId}"
             controls
             preload="metadata"
             aria-label="${label} audio player">
        <source src="${mediaUrl}" type="audio/mpeg" />
        Your browser does not support the audio element.
      </audio>
      <div class="visualiser-bar">
        <div class="visualiser-fill" id="${fillId}"></div>
      </div>
    </div>
    <div class="player-actions">
      <a href="${mediaUrl}"
         download="${filename}"
         class="btn btn-download"
         aria-label="Download ${label} MP3">
        ⬇ Download MP3
      </a>
      <button
        id="regenBtn-${type}"
        onclick="triggerRegenAudio('${dateStr}', '${type}')"
        title="Re-run LLM + TTS for this date (no full re-scrape)"
        style="padding:0 0.85rem; height:36px; font-size:0.78rem; font-weight:600;
               background:var(--ctp-surface0); color:var(--ctp-text);
               border:1px solid var(--ctp-surface1); border-radius:8px;
               cursor:pointer; display:inline-flex; align-items:center; gap:0.4rem;
               transition:background 0.15s;"
        onmouseover="this.style.background='var(--ctp-surface1)'"
        onmouseout="this.style.background='var(--ctp-surface0)'">
        🔁 Re-generate
      </button>
    </div>
  `;

  // Wire up progress fill bar
  const audio = $(audioId);
  const fill  = $(fillId);
  if (audio && fill) {
    audio.addEventListener('timeupdate', () => {
      if (audio.duration) {
        fill.style.width = `${(audio.currentTime / audio.duration) * 100}%`;
      }
    });
    audio.addEventListener('ended', () => { fill.style.width = '0%'; });
  }
}

// ── Refresh button ───────────────────────────────────────────────
refreshBtn.addEventListener('click', () => loadMediaList(false));
