// ── Scraper triggering, status polling, and log rendering ───────
//
// One fetch/render path serves both log surfaces (inline console and the
// logs modal); the status poll drives all UI state transitions.
import { $, toast, setStatus } from './utils.js';
import { saveSettings, getSelectedSources } from './settings.js';
import { loadMediaList } from './playlist.js';

export let isScrapingActive = false;

const scrapeBtn               = $('scrapeBtn');
const scrapeProgressBar       = $('scrapeProgressBar');
const scrapeFailureBanner     = $('scrapeFailureBanner');
const scraperLogConsole       = $('scraperLogConsole');
const consoleLogOutput        = $('consoleLogOutput');
const toggleConsoleBtn        = $('toggleConsoleBtn');
const consoleStatusDot        = $('consoleStatusDot');
const logsModal               = $('logsModal');
const modalConsoleLogOutput   = $('modalConsoleLogOutput');
const modalConsoleStatusDot   = $('modalConsoleStatusDot');

let isConsoleCollapsed = false;

toggleConsoleBtn.addEventListener('click', () => {
  isConsoleCollapsed = !isConsoleCollapsed;
  if (isConsoleCollapsed) {
    consoleLogOutput.style.display = 'none';
    toggleConsoleBtn.innerHTML = 'Expand';
  } else {
    consoleLogOutput.style.display = 'block';
    toggleConsoleBtn.innerHTML = 'Collapse';
    consoleLogOutput.scrollTop = consoleLogOutput.scrollHeight;
  }
});

$('dismissScrapeFailureBtn').addEventListener('click', () => {
  scrapeFailureBanner.style.display = 'none';
});

function expandConsole() {
  if (isConsoleCollapsed) {
    isConsoleCollapsed = false;
    consoleLogOutput.style.display = 'block';
    toggleConsoleBtn.innerHTML = 'Collapse';
  }
}

/// Fetch the scraper logs once and render into every visible surface.
async function refreshLogs() {
  try {
    const resp = await fetch('/api/scrape/logs');
    if (!resp.ok) return;
    const lines = await resp.json();
    const text = lines.join('\n');
    if (consoleLogOutput.textContent !== text) {
      consoleLogOutput.textContent = text;
      consoleLogOutput.scrollTop = consoleLogOutput.scrollHeight;
    }
    if (logsModal.style.display === 'flex' && modalConsoleLogOutput.textContent !== text) {
      modalConsoleLogOutput.textContent = text;
      modalConsoleLogOutput.scrollTop = modalConsoleLogOutput.scrollHeight;
    }
  } catch (err) {
    console.error('Failed to fetch scraper logs:', err);
  }
}

function updateStatusDots(running, lastRunSuccess) {
  const color = running ? 'var(--ctp-yellow)'
    : lastRunSuccess ? 'var(--ctp-green)' : 'var(--ctp-red)';
  consoleStatusDot.style.background = color;
  modalConsoleStatusDot.style.background = color;
}

function showRunningUi(statusMsg) {
  isScrapingActive = true;
  scrapeBtn.disabled = true;
  scrapeBtn.innerHTML = '⚙️ Running...';
  scrapeProgressBar.style.display = 'block';
  scrapeFailureBanner.style.display = 'none';
  scraperLogConsole.style.display = 'block';
  updateStatusDots(true, true);
  setStatus(statusMsg, 'loading');
}

export async function checkScraperStatus() {
  try {
    const resp = await fetch('/api/scrape/status');
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const data = await resp.json();

    if (data.running) {
      showRunningUi('News Station is actively compiling & curating...');
      await refreshLogs();
    } else {
      updateStatusDots(false, data.last_run_success);
      // If it was running but just stopped
      if (isScrapingActive) {
        isScrapingActive = false;
        scrapeBtn.disabled = false;
        scrapeBtn.innerHTML = '⚡ Run Scraper';
        scrapeProgressBar.style.display = 'none';

        await refreshLogs();

        if (data.last_run_success) {
          scraperLogConsole.style.display = 'none';
          scrapeFailureBanner.style.display = 'none';
          toast('Daily news run completed successfully!', 'success');
          loadMediaList(false);
        } else {
          scrapeFailureBanner.style.display = 'block';
          scraperLogConsole.style.display = 'block';
          expandConsole();
          toast('Daily news run failed!', 'error');
        }
      } else {
        // Idle state on initial load
        if (!data.last_run_success) {
          scrapeFailureBanner.style.display = 'block';
          scraperLogConsole.style.display = 'block';
          await refreshLogs();
        } else {
          scrapeFailureBanner.style.display = 'none';
        }
      }
    }
  } catch (err) {
    console.error('Failed to query scraper status:', err);
  }
}

// ── Manual Scrape Trigger ────────────────────────────────────────
async function triggerScraper() {
  if (isScrapingActive) return;

  saveSettings();

  const shortSources = getSelectedSources('short');
  const longSources = getSelectedSources('long');

  if (shortSources.length === 0 && longSources.length === 0) {
    toast('Select at least one source for short briefing or long podcast.', 'error');
    return;
  }

  const qParams = new URLSearchParams({
    voice_short: $('voiceShortSelect').value,
    voice_long: $('voiceLongSelect').value,
    short_sources: shortSources.join(','),
    long_sources: longSources.join(',')
  });

  try {
    scrapeBtn.disabled = true;
    const resp = await fetch(`/api/scrape/trigger?${qParams.toString()}`, { method: 'POST' });
    if (resp.status === 409) {
      toast('Scraper pipeline is already running.', 'error');
      return;
    }
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);

    toast('Scraper run triggered successfully!', 'success');
    checkScraperStatus();
  } catch (err) {
    toast(`Failed to trigger scraper: ${err.message}`, 'error');
    scrapeBtn.disabled = false;
  }
}

scrapeBtn.addEventListener('click', triggerScraper);

// ── Re-generate Audio (LLM + TTS only, no full scrape) ───────────
async function triggerRegenAudio(dateStr, type) {
  if (isScrapingActive) {
    toast('A scraper job is already running. Please wait.', 'error');
    return;
  }

  const btn = document.getElementById(`regenBtn-${type}`);
  if (btn) {
    btn.disabled = true;
    btn.innerHTML = '⏳ Regenerating…';
  }

  const shortSources = getSelectedSources('short');
  const longSources = getSelectedSources('long');

  const qParams = new URLSearchParams({ date: dateStr });
  // Only regenerate the clicked card's track — the backend leaves the
  // other track's MP3 untouched and the LLM only writes one script.
  if (type === 'radio' || type === 'podcast') qParams.set('track', type);
  qParams.set('voice_short', $('voiceShortSelect').value);
  qParams.set('voice_long', $('voiceLongSelect').value);
  if (shortSources.length) qParams.set('short_sources', shortSources.join(','));
  if (longSources.length)  qParams.set('long_sources',  longSources.join(','));

  try {
    const resp = await fetch(`/api/scrape/regen-audio?${qParams.toString()}`, { method: 'POST' });
    if (resp.status === 409) {
      toast('A scraper job is already running.', 'error');
      if (btn) { btn.disabled = false; btn.innerHTML = '🔁 Re-generate'; }
      return;
    }
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);

    // Immediately show the same running UI as a full scrape
    showRunningUi('Re-generating audio — LLM + TTS running...');
    expandConsole();
    toast('Re-generating audio — LLM + TTS running, no full re-scrape.', 'success');
    await refreshLogs();
  } catch (err) {
    toast(`Failed to start audio regen: ${err.message}`, 'error');
    if (btn) { btn.disabled = false; btn.innerHTML = '🔁 Re-generate'; }
  }
}

// Exposed for the inline onclick handlers on dynamically rendered
// player cards (see playlist.js renderAudioPlayer)
window.triggerRegenAudio = triggerRegenAudio;

// ── Logs Modal Overlay ───────────────────────────────────────────
let logsModalInterval = null;

function openLogsModal() {
  logsModal.style.display = 'flex';
  refreshLogs();
  logsModalInterval = setInterval(refreshLogs, 2000);
}

function closeLogsModal() {
  logsModal.style.display = 'none';
  if (logsModalInterval) {
    clearInterval(logsModalInterval);
    logsModalInterval = null;
  }
}

$('showLogsModalBtn').addEventListener('click', openLogsModal);
$('closeLogsModalBtn').addEventListener('click', closeLogsModal);
$('closeLogsModalOkBtn').addEventListener('click', closeLogsModal);
$('modalLogsRefreshBtn').addEventListener('click', refreshLogs);

// Poll status every 3 seconds
setInterval(checkScraperStatus, 3000);
