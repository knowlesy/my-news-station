// ── Shared DOM + formatting helpers ─────────────────────────────

export const $ = id => document.getElementById(id);

export function toast(message, type = '') {
  const el = document.createElement('div');
  el.className = `toast ${type}`;
  el.textContent = message;
  $('toastContainer').appendChild(el);
  setTimeout(() => el.remove(), 4200);
}

export function setStatus(msg, state = '') {
  $('statusText').textContent = msg;
  $('statusDot').className = `status-dot ${state}`;
}

// ── Format YYYYMMDD / YYYYMMDD-HHMMSS → human readable ──────────
export function formatDate(dateStr) {
  if (!dateStr) return dateStr;
  if (dateStr.length === 15) {
    const y = dateStr.slice(0, 4);
    const m = dateStr.slice(4, 6);
    const d = dateStr.slice(6, 8);
    const hh = dateStr.slice(9, 11);
    const mm = dateStr.slice(11, 13);
    const dt = new Date(`${y}-${m}-${d}T${hh}:${mm}:00`);
    const dStr = dt.toLocaleDateString('en-GB', { weekday:'short', year:'numeric', month:'short', day:'numeric' });
    return `${dStr} · ${hh}:${mm}`;
  }
  if (dateStr.length === 8) {
    const y = dateStr.slice(0, 4);
    const m = dateStr.slice(4, 6);
    const d = dateStr.slice(6, 8);
    const dt = new Date(`${y}-${m}-${d}`);
    return dt.toLocaleDateString('en-GB', { weekday:'short', year:'numeric', month:'short', day:'numeric' });
  }
  return dateStr;
}

// ── Check if edition is new (<12h old & not clicked) ─────────────
export function isNewEdition(dateStr) {
  if (!dateStr || dateStr.length !== 15) return false;

  const clicked = JSON.parse(localStorage.getItem('clickedEditions') || '[]');
  if (clicked.includes(dateStr)) return false;

  try {
    const y = dateStr.slice(0, 4);
    const m = dateStr.slice(4, 6);
    const d = dateStr.slice(6, 8);
    const hh = dateStr.slice(9, 11);
    const mm = dateStr.slice(11, 13);
    const dt = new Date(`${y}-${m}-${d}T${hh}:${mm}:00`);
    const ageHours = (new Date() - dt) / (1000 * 60 * 60);
    return ageHours >= 0 && ageHours < 12;
  } catch (e) {
    return false;
  }
}
