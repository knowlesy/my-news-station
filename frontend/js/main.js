// ── Application bootstrap ────────────────────────────────────────
import { fetchAndRenderConfig } from './settings.js';
import { loadMediaList } from './playlist.js';
import { checkScraperStatus } from './scraperStatus.js';

fetchAndRenderConfig();
loadMediaList(false);
checkScraperStatus();
