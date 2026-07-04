// ── Application bootstrap ────────────────────────────────────────
import { fetchAndRenderConfig } from './settings.js';
import { loadMediaList } from './playlist.js';
import { checkScraperStatus } from './scraperStatus.js';

// Check for new releases on startup
async function checkForUpgrade() {
  try {
    const versionRes = await fetch('/api/version');
    const { version: current } = await versionRes.json();

    const releaseRes = await fetch('https://api.github.com/repos/knowlesy/my-news-station/releases/latest');
    if (!releaseRes.ok) return;

    const { tag_name: latest } = await releaseRes.json();
    if (latest && latest !== `v${current}` && current !== '0.1.0') {
      const banner = document.createElement('div');
      banner.style.cssText = 'position:fixed; top:0; left:0; right:0; background:#ff9800; color:#fff; padding:12px; text-align:center; z-index:1000; font-weight:bold;';
      banner.innerHTML = `📦 New version available: ${latest} <a href="https://github.com/knowlesy/my-news-station/releases/latest" target="_blank" style="color:#fff; text-decoration:underline; margin-left:12px;">View release</a>`;
      document.body.insertBefore(banner, document.body.firstChild);
    }
  } catch (err) {
    console.log('Version check skipped:', err);
  }
}

fetchAndRenderConfig();
loadMediaList(false);
checkScraperStatus();
checkForUpgrade();
