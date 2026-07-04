// ── Theme, sources config, voices, and the Options modal ────────
//
// All user settings live in server-side config.json (fetched into
// `currentConfig`); only cosmetic prefs (theme) and the clicked-editions
// list stay in localStorage.
import { $, toast } from './utils.js';
import {
  syncFromConfig, getDeviceSaveState, renderCrosspointDevices,
  updateSendButton, loadSentHistory,
} from './crosspoint.js';

// ── Theme ────────────────────────────────────────────────────────
const themeSelect = $('themeSelect');

function applyTheme(theme) {
  document.documentElement.setAttribute('data-theme', theme);
  localStorage.setItem('ctp-theme', theme);
  themeSelect.value = theme;
}

themeSelect.addEventListener('change', () => applyTheme(themeSelect.value));

// Restore persisted theme, default = mocha
applyTheme(localStorage.getItem('ctp-theme') || 'mocha');

// ── Config state ─────────────────────────────────────────────────
export let currentConfig = { rss_feeds: [], medium_tags: [] };
let sourceActivity = {};

/// POST a partial config update, merged over the latest known config.
async function postConfig(partial) {
  const payload = { ...currentConfig, ...partial };
  const res = await fetch('/api/config', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload),
  });
  if (!res.ok) throw new Error('Failed to save config');
  currentConfig = payload;
}

// ── Voice + per-briefing source selection ────────────────────────
// (config.json-backed; falls back to legacy localStorage values once)
export function loadSettings() {
  $('skipPaywalledCheckbox').checked = currentConfig.skip_paywalled_posts !== false;

  $('voiceShortSelect').value = currentConfig.voice_short
    || localStorage.getItem('voice_short') || 'en-GB-SoniaNeural';
  $('voiceLongSelect').value = currentConfig.voice_long
    || localStorage.getItem('voice_long') || 'en-US-GuyNeural';

  const savedShort = currentConfig.sources_short?.length
    ? currentConfig.sources_short.join(',')
    : localStorage.getItem('sources_short');
  const savedLong = currentConfig.sources_long?.length
    ? currentConfig.sources_long.join(',')
    : localStorage.getItem('sources_long');

  document.querySelectorAll('#shortSourcesContainer input[type="checkbox"]').forEach(cb => {
    if (savedShort !== null) {
      cb.checked = savedShort.split(',').includes(cb.value);
    } else {
      cb.checked = true;
    }
  });

  document.querySelectorAll('#longSourcesContainer input[type="checkbox"]').forEach(cb => {
    if (savedLong !== null) {
      cb.checked = savedLong.split(',').includes(cb.value);
    } else {
      cb.checked = true;
    }
  });

  updateSelectAllState('short');
  updateSelectAllState('long');
}

export function getSelectedSources(type) {
  const containerId = type === 'short' ? 'shortSourcesContainer' : 'longSourcesContainer';
  return Array.from(
    document.querySelectorAll(`#${containerId} input[type="checkbox"]:checked`)
  ).map(cb => cb.value);
}

export async function saveSettings() {
  try {
    await postConfig({
      skip_paywalled_posts: $('skipPaywalledCheckbox').checked,
      voice_short: $('voiceShortSelect').value,
      voice_long: $('voiceLongSelect').value,
      sources_short: getSelectedSources('short'),
      sources_long: getSelectedSources('long'),
    });
  } catch (err) {
    console.error('Error saving settings:', err);
    toast('Failed to save settings to server.', 'error');
  }
}

[$('voiceShortSelect'), $('voiceLongSelect')].forEach(el => {
  el.addEventListener('change', saveSettings);
});

// ── Card-level settings and TTS Preview ──────────────────────────
let activeAudioSample = null;

window.toggleCardSettings = function(type) {
  const drawer = $(`${type}Settings`);
  const content = $(`${type}Content`);
  if (!drawer) return;
  if (drawer.style.display === 'none') {
    drawer.style.display = 'flex';
    if (content) content.style.display = 'none';
  } else {
    drawer.style.display = 'none';
    if (content) content.style.display = 'block';
  }
};

window.playVoiceSample = function(type) {
  const select = type === 'radio' ? $('voiceShortSelect') : $('voiceLongSelect');
  const voice = select.value;

  if (activeAudioSample) {
    activeAudioSample.pause();
    activeAudioSample = null;
  }

  const name = voice.split('-').pop().replace('Neural', '');
  toast(`Requesting sample for ${name}…`, 'info');

  const audio = new Audio(`/api/tts/preview?voice=${encodeURIComponent(voice)}`);
  activeAudioSample = audio;

  audio.play().then(() => {
    toast(`Playing sample for ${name}`, 'success');
  }).catch(err => {
    console.error('Failed to play TTS preview:', err);
    toast('Failed to load or play voice sample.', 'error');
  });
};

// ── Config fetch + source checkbox rendering ─────────────────────
export async function fetchAndRenderConfig() {
  try {
    const res = await fetch('/api/config');
    if (!res.ok) throw new Error('Failed to fetch config');
    currentConfig = await res.json();

    try {
      const actRes = await fetch('/api/sources/activity');
      if (actRes.ok) {
        sourceActivity = await actRes.json();
      }
    } catch (e) {
      console.warn('Failed to fetch source activity:', e);
    }

    const sources = [];
    currentConfig.rss_feeds.forEach(feed => {
      sources.push(feed.name);
    });
    currentConfig.medium_tags.forEach(tag => {
      sources.push(`Medium/tags/${tag}`);
    });

    renderSourceCheckboxes('short', sources);
    renderSourceCheckboxes('long', sources);

    syncFromConfig(currentConfig);
    updateSendButton();

    loadSettings();
  } catch (err) {
    console.error('Error fetching config:', err);
    toast('Failed to load sources list from server.', 'error');
  }
}

function renderSourceCheckboxes(type, sources) {
  const container = $(`${type}SourcesContainer`);
  if (!container) return;
  container.innerHTML = '';

  if (sources.length === 0) {
    container.innerHTML = '<div style="color:var(--ctp-subtext0); font-style:italic;">No sources configured</div>';
    return;
  }

  const sorted = [...sources].sort((a, b) => a.localeCompare(b, undefined, { sensitivity: 'base' }));
  sorted.forEach(src => {
    const label = document.createElement('label');
    label.style.display = 'flex';
    label.style.alignItems = 'center';
    label.style.gap = '0.4rem';
    label.style.color = 'var(--ctp-text)';
    label.style.cursor = 'pointer';
    label.style.fontSize = '0.75rem';

    const cb = document.createElement('input');
    cb.type = 'checkbox';
    cb.value = src;
    cb.style.cursor = 'pointer';
    cb.style.width = '14px';
    cb.style.height = '14px';
    cb.addEventListener('change', () => {
      saveSettings();
      updateSelectAllState(type);
    });

    let warningIcon = '';
    if (!currentConfig.silenced_sources?.includes(src)) {
       const entry = sourceActivity[src];
       const lastSeen = typeof entry === 'string' ? entry : entry?.last_seen;
       const degraded = typeof entry === 'object' && entry?.degraded;
       if (degraded) {
         warningIcon = '<span title="Full articles blocked upstream — using RSS summary only." style="cursor:help;">🟡</span>';
       } else if (lastSeen) {
         const diffDays = (new Date() - new Date(lastSeen)) / (1000 * 60 * 60 * 24);
         if (diffDays > 30) {
           warningIcon = '<span title="No articles pulled for >30 days. Possibly dead." style="cursor:help;">⚠️</span>';
         }
       }
    }

    label.appendChild(cb);
    label.insertAdjacentHTML('beforeend', `<span>${src}</span> ${warningIcon}`);
    container.appendChild(label);
  });
}

function updateSelectAllState(type) {
  const selectAll = type === 'short' ? $('selectAllShort') : $('selectAllLong');
  const containerId = type === 'short' ? 'shortSourcesContainer' : 'longSourcesContainer';
  const cbs = document.querySelectorAll(`#${containerId} input[type="checkbox"]`);
  if (cbs.length === 0) return;

  const allChecked = Array.from(cbs).every(cb => cb.checked);
  selectAll.checked = allChecked;
}

$('selectAllShort').addEventListener('change', (e) => {
  const checked = e.target.checked;
  document.querySelectorAll('#shortSourcesContainer input[type="checkbox"]').forEach(cb => {
    cb.checked = checked;
  });
  saveSettings();
});

$('selectAllLong').addEventListener('change', (e) => {
  const checked = e.target.checked;
  document.querySelectorAll('#longSourcesContainer input[type="checkbox"]').forEach(cb => {
    cb.checked = checked;
  });
  saveSettings();
});

// ── Global Options Modal ─────────────────────────────────────────
const optionsModal = $('optionsModal');

function renderSourceHealth() {
  const container = $('sourceHealthContainer');
  if (!container) return;
  container.innerHTML = '';

  const allSources = [
    ...currentConfig.rss_feeds.map(f => typeof f === 'string' ? f : f.name),
    ...currentConfig.medium_tags.map(t => `Medium/tags/${t}`)
  ].sort((a, b) => a.localeCompare(b, undefined, { sensitivity: 'base' }));

  if (allSources.length === 0) {
    container.innerHTML = '<div style="color:var(--ctp-subtext0); font-style:italic; font-size:0.75rem;">No sources configured</div>';
    return;
  }

  allSources.forEach(src => {
    const entry = sourceActivity[src];
    const lastSeenStr = typeof entry === 'string' ? entry : entry?.last_seen;
    const degraded = typeof entry === 'object' && entry?.degraded;
    let statusHtml = '<span style="color:var(--ctp-subtext0);" title="Never seen">⚪ Unknown</span>';
    let isDead = false;

    if (lastSeenStr) {
      const lastSeenDate = new Date(lastSeenStr);
      const diffDays = (new Date() - lastSeenDate) / (1000 * 60 * 60 * 24);

      if (diffDays > 30) {
        statusHtml = `<span style="color:var(--ctp-red); font-weight:600;" title="Last seen ${Math.floor(diffDays)} days ago">🔴 Dead</span>`;
        isDead = true;
      } else if (degraded) {
        statusHtml = `<span style="color:var(--ctp-yellow); font-weight:600;" title="Full articles blocked upstream (network-level) — using RSS summary only, last seen ${Math.floor(diffDays)} days ago">🟡 Degraded</span>`;
      } else {
        statusHtml = `<span style="color:var(--ctp-green);" title="Last seen ${Math.floor(diffDays)} days ago">🟢 Active</span>`;
      }
    }

    const isSilenced = currentConfig.silenced_sources?.includes(src);

    const row = document.createElement('div');
    row.style.display = 'flex';
    row.style.justifyContent = 'space-between';
    row.style.alignItems = 'center';
    row.style.fontSize = '0.75rem';
    row.style.borderBottom = '1px solid var(--ctp-surface0)';
    row.style.paddingBottom = '0.25rem';

    row.innerHTML = `
      <div style="display:flex; gap:0.5rem; align-items:center; flex:1; overflow:hidden;">
        ${statusHtml}
        <span style="color:var(--ctp-text); white-space:nowrap; overflow:hidden; text-overflow:ellipsis;" title="${src}">${src}</span>
      </div>
      <label style="display:flex; gap:0.25rem; align-items:center; cursor:pointer; color:var(--ctp-subtext0);">
        <input type="checkbox" class="silence-checkbox" value="${src}" ${isSilenced ? 'checked' : ''}> Silence
      </label>
    `;
    container.appendChild(row);
  });
}

async function loadVersionDisplay() {
  const el = $('versionDisplay');
  if (!el) return;
  try {
    const res = await fetch('/api/version');
    if (!res.ok) throw new Error('Failed to fetch version');
    const { git_sha, build_date } = await res.json();
    const shortSha = git_sha && git_sha !== 'dev' ? git_sha.slice(0, 7) : git_sha;
    el.textContent = `Build: ${build_date} (${shortSha})`;
  } catch (err) {
    console.warn('Failed to load version info:', err);
    el.textContent = '';
  }
}

function toggleOptionsModal() {
  if (optionsModal.style.display === 'none' || !optionsModal.style.display) {
    const feedsUrls = currentConfig.rss_feeds.map(f => typeof f === 'string' ? f : f.url).join('\n');
    $('rssFeedsInput').value = feedsUrls;
    $('mediumTagsInput').value = currentConfig.medium_tags.join(', ');
    $('systemPromptInput').value = currentConfig.system_prompt || '';
    renderSourceHealth();
    syncFromConfig(currentConfig);
    renderCrosspointDevices();
    loadVersionDisplay();
    optionsModal.style.display = 'flex';
  } else {
    optionsModal.style.display = 'none';
  }
}

$('optionsBtn').addEventListener('click', toggleOptionsModal);
$('closeOptionsBtn').addEventListener('click', toggleOptionsModal);
$('cancelOptionsBtn').addEventListener('click', toggleOptionsModal);

// Close modal when clicking outside (on overlay/background)
optionsModal.addEventListener('click', (e) => {
  if (e.target === optionsModal) {
    optionsModal.style.display = 'none';
  }
});

const DEFAULT_SYSTEM_PROMPT = `You are an advanced news editing and broadcasting engine with a distinct personality. You are provided with a complete daily pool of articles, along with a marked list of curated audio highlights. Process this text and wrap your outputs in these designated XML tags:

TONE RULES (apply to both outputs):
- For tech, business, and general news: be dry, witty, and a little sardonic. You have opinions. If a company has released its fifth "revolutionary" AI product this month, you may note that. If a framework has broken its API again, you can say so. Keep it sharp but never mean-spirited.
- For stories involving death, serious injury, war, disaster, mental health, or human tragedy: drop the wit entirely. Shift to a calm, respectful, measured tone. Acknowledge the weight of the story before moving on. Never make light of suffering.
- The tonal shift should feel deliberate and human — like a presenter who knows when to be funny and when to shut up and be decent.

- <short_radio>: Focus ONLY on the curated stories listed under the 'For <short_radio>' section below. Do NOT mention, summarize, or draw details from any other articles in the pool. Deliver a punchy flash briefing — think smart morning radio, not a press release.
  CRITICAL TIME CONSTRAINT: This script MUST be crisp and tightly edited. It must absolutely NOT exceed a 30-minute reading time under any circumstances. Target a high-density delivery between 500 to 2,000 words total.
  Structure: Continuous text script. Group stories by source and introduce them naturally (e.g., 'From BBC News...' or 'Over at GitHub...') instead of repeating '[Source] reported that' for each story. Keep it flowing and conversational. NO markdown formatting, asterisks, or bolding. Translate code/terminal commands into conceptual, easy-to-understand audio explanations.
  Always use time-neutral greetings (e.g., 'Hello', 'Welcome', 'Right then — here is what happened') rather than 'Good morning' or 'Good evening'.

- <long_podcast>: Focus ONLY on the curated stories listed under the 'For <long_podcast>' section below. Do NOT mention, summarize, or draw details from any other articles in the pool. Act as a sharp, conversational tech podcast host — one who has read the room, done the reading, and isn't afraid to say what they actually think. Seamless monologue, smooth transitions, no speaker tags or audio cues. Pure text prose optimized for TTS.
  Always use time-neutral greetings (e.g., 'Hello', 'Welcome back', 'Here we go again') rather than 'Good morning' or 'Good evening'.`;

$('resetPromptBtn').addEventListener('click', () => {
  $('systemPromptInput').value = DEFAULT_SYSTEM_PROMPT;
});

$('saveOptionsBtn').addEventListener('click', async () => {
  const feedsText = $('rssFeedsInput').value.trim();
  const tagsText = $('mediumTagsInput').value.trim();

  const rss_feeds = feedsText.split('\n')
    .map(url => url.trim())
    .filter(url => url.length > 0);

  const medium_tags = tagsText.split(',')
    .map(tag => tag.trim())
    .filter(tag => tag.length > 0);

  const silenced_sources = Array.from(document.querySelectorAll('#sourceHealthContainer .silence-checkbox:checked')).map(cb => cb.value);
  const system_prompt = $('systemPromptInput').value.trim() || null;

  try {
    await postConfig({
      rss_feeds: rss_feeds.map(url => {
        try {
          const hostname = new URL(url).hostname;
          const friendly = hostname.replace('www.', '');
          return { name: friendly, url: url };
        } catch(e) {
          return { name: "News Feed", url: url };
        }
      }),
      medium_tags: medium_tags,
      silenced_sources: silenced_sources,
      system_prompt: system_prompt,
      ...getDeviceSaveState(),
    });

    toast('Configuration saved successfully', 'success');
    optionsModal.style.display = 'none';

    await fetchAndRenderConfig();
  } catch (err) {
    console.error('Error saving config:', err);
    toast('Failed to save configuration to server.', 'error');
  }
});

// Fetch the send history once at startup so the send button state is accurate
loadSentHistory();
