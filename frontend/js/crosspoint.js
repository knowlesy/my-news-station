// ── Crosspoint X4 device manager + Send-to-X4 button ────────────
import { $, toast } from './utils.js';
import { activeDate } from './playlist.js';

// In-memory state, synced from currentConfig by settings.js
let crosspointDevices   = [];  // [{name, ip}]
let defaultCrosspointIp = null;
let deviceProbeCache    = {};  // ip -> {status, firmware, ts}
let sentHistory         = {};  // "YYYYMMDD_ip" -> iso timestamp
let isFirstDeviceAdd    = true;

export async function loadSentHistory() {
  try {
    const r = await fetch('/api/crosspoint/history');
    if (r.ok) sentHistory = await r.json();
  } catch(e) { /* non-fatal */ }
}

/// Adopt device state from a freshly fetched config.
export function syncFromConfig(cfg) {
  crosspointDevices   = (cfg.crosspoint_devices || []).map(d => ({...d}));
  defaultCrosspointIp = cfg.default_crosspoint_ip || null;
  isFirstDeviceAdd    = crosspointDevices.length === 0;
}

/// Device fields for the config save payload.
export function getDeviceSaveState() {
  return {
    crosspoint_devices: crosspointDevices,
    default_crosspoint_ip: defaultCrosspointIp,
  };
}

async function probeDevice(ip) {
  const cached = deviceProbeCache[ip];
  if (cached && (Date.now() - cached.ts) < 15000) return cached;
  try {
    const r = await fetch(`/api/crosspoint/probe?ip=${encodeURIComponent(ip)}`);
    const data = r.ok ? await r.json() : { status: 'offline', firmware: null };
    deviceProbeCache[ip] = { ...data, ts: Date.now() };
    return deviceProbeCache[ip];
  } catch(e) {
    return { status: 'offline', firmware: null, ts: Date.now() };
  }
}

function statusLight(status) {
  if (status === 'online_crosspoint' || status === 'online_stock') return '🟢';
  if (status === 'offline') return '🔴';
  return '⚪';
}

export async function renderCrosspointDevices() {
  const container = $('crosspointDeviceList');
  if (!container) return;

  if (crosspointDevices.length === 0) {
    container.innerHTML = '<div style="font-size:0.72rem; color:var(--ctp-subtext0); font-style:italic;">No devices configured — add one below.</div>';
    return;
  }

  container.innerHTML = '';
  for (const dev of crosspointDevices) {
    const row = document.createElement('div');
    row.style.cssText = 'display:flex; align-items:center; gap:0.5rem; font-size:0.75rem; background:var(--ctp-base); border:1px solid var(--ctp-surface0); border-radius:6px; padding:0.35rem 0.5rem;';

    const probe = deviceProbeCache[dev.ip] || { status: 'unknown' };
    const light = statusLight(probe.status);
    const isDefault = dev.ip === defaultCrosspointIp;

    row.innerHTML = `
      <span id="light-${CSS.escape(dev.ip)}" title="${probe.status}" style="font-size:0.9rem; cursor:default;">${light}</span>
      <span style="flex:1; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; color:var(--ctp-text);" title="${dev.ip}">${dev.name} <span style="color:var(--ctp-subtext0); font-family:monospace;">${dev.ip}</span></span>
      ${isDefault ? '<span style="font-size:0.65rem; background:var(--ctp-blue); color:var(--ctp-base); border-radius:4px; padding:1px 5px;">default</span>' : `<button class="btn set-default-btn" data-ip="${dev.ip}" style="font-size:0.65rem; height:22px; padding:0 6px; background:var(--ctp-surface0); color:var(--ctp-subtext0);">Set default</button>`}
      <button class="btn retry-probe-btn" data-ip="${dev.ip}" style="font-size:0.65rem; height:22px; padding:0 6px; background:var(--ctp-surface1); color:var(--ctp-text);" title="Check connectivity">↺</button>
      <button class="btn remove-device-btn" data-ip="${dev.ip}" style="font-size:0.65rem; height:22px; padding:0 6px; background:transparent; color:var(--ctp-red);" title="Remove device">✕</button>
    `;
    container.appendChild(row);

    // Kick off probe in background — update light when done
    probeDevice(dev.ip).then(result => {
      const el = document.getElementById(`light-${CSS.escape(dev.ip)}`);
      if (el) {
        el.textContent = statusLight(result.status);
        el.title = result.status;
      }
    });
  }

  container.querySelectorAll('.set-default-btn').forEach(btn => {
    btn.addEventListener('click', () => {
      defaultCrosspointIp = btn.dataset.ip;
      renderCrosspointDevices();
      updateSendButton();
    });
  });
  container.querySelectorAll('.retry-probe-btn').forEach(btn => {
    btn.addEventListener('click', async () => {
      const ip = btn.dataset.ip;
      delete deviceProbeCache[ip];
      btn.textContent = '…';
      await probeDevice(ip);
      renderCrosspointDevices();
      updateSendButton();
    });
  });
  container.querySelectorAll('.remove-device-btn').forEach(btn => {
    btn.addEventListener('click', () => {
      crosspointDevices = crosspointDevices.filter(d => d.ip !== btn.dataset.ip);
      if (defaultCrosspointIp === btn.dataset.ip) {
        defaultCrosspointIp = crosspointDevices[0]?.ip || null;
      }
      renderCrosspointDevices();
      updateSendButton();
    });
  });
}

$('addDeviceBtn').addEventListener('click', async () => {
  const name = $('newDeviceName').value.trim();
  const ip   = $('newDeviceIp').value.trim();
  if (!name || !ip) { toast('Enter both a name and an IP address.', 'error'); return; }
  if (crosspointDevices.find(d => d.ip === ip)) { toast('A device with that IP already exists.', 'error'); return; }

  crosspointDevices.push({ name, ip });
  if (!defaultCrosspointIp) defaultCrosspointIp = ip;
  $('newDeviceName').value = '';
  $('newDeviceIp').value = '';

  if (isFirstDeviceAdd) {
    isFirstDeviceAdd = false;
    toast('💡 Open the Crosspoint app on your X4 device to enable file receive before sending.', 'info');
  }
  await renderCrosspointDevices();
  updateSendButton();
});

// ── Send to X4 button state machine ──────────────────────────────
const sendBtn = $('sendToX4Btn');

export function updateSendButton() {
  if (!sendBtn) return;
  const defaultDev = crosspointDevices.find(d => d.ip === defaultCrosspointIp);

  if (!defaultDev) {
    sendBtn.style.display = 'none';
    return;
  }
  sendBtn.style.display = '';

  if (!activeDate) {
    sendBtn.textContent = '📤 Send to X4';
    sendBtn.disabled = true;
    sendBtn.style.background = 'var(--ctp-surface1)';
    sendBtn.style.color = 'var(--ctp-subtext0)';
    sendBtn.title = 'Select an edition first';
    return;
  }

  const histKey = `${activeDate}_${defaultCrosspointIp}`;
  const probe   = deviceProbeCache[defaultCrosspointIp] || { status: 'unknown' };

  if (sentHistory[histKey]) {
    sendBtn.textContent = '✅ Already on device';
    sendBtn.disabled = false;
    sendBtn.style.background = 'var(--ctp-surface0)';
    sendBtn.style.color = 'var(--ctp-green)';
    sendBtn.title = `Sent ${new Date(sentHistory[histKey]).toLocaleString()} — tap to re-send`;
  } else if (probe.status === 'offline') {
    sendBtn.textContent = '🔴 Unable to connect';
    sendBtn.disabled = true;
    sendBtn.style.background = 'var(--ctp-surface1)';
    sendBtn.style.color = 'var(--ctp-red)';
    sendBtn.title = `Cannot reach ${defaultCrosspointIp} — open the app on the X4 device`;
  } else {
    sendBtn.textContent = '📤 Send to X4';
    sendBtn.disabled = false;
    sendBtn.style.background = 'var(--ctp-mauve)';
    sendBtn.style.color = 'var(--ctp-base)';
    sendBtn.title = `Send EPUB to ${defaultDev.name} (${defaultCrosspointIp})`;
  }
}

/// Called when a playlist date is selected: re-probe the default device
/// and refresh the button state.
export function refreshSendButtonForDate() {
  if (defaultCrosspointIp) {
    delete deviceProbeCache[defaultCrosspointIp];
    probeDevice(defaultCrosspointIp).then(() => updateSendButton());
  }
  updateSendButton();
}

sendBtn.addEventListener('click', async () => {
  const defaultDev = crosspointDevices.find(d => d.ip === defaultCrosspointIp);
  if (!defaultDev || !activeDate) return;

  const histKey = `${activeDate}_${defaultCrosspointIp}`;
  if (sentHistory[histKey]) {
    // Already sent — ask to re-send
    if (!confirm(`This edition was already sent to ${defaultDev.name} on ${new Date(sentHistory[histKey]).toLocaleString()}.\n\nSend again?`)) return;
    delete sentHistory[histKey]; // allow re-send
  }

  // Probe first to get firmware type
  sendBtn.textContent = '⟳ Checking…';
  sendBtn.disabled = true;
  delete deviceProbeCache[defaultCrosspointIp];
  const probe = await probeDevice(defaultCrosspointIp);

  if (probe.status === 'offline') {
    toast(`🔴 Cannot reach ${defaultDev.name} — open the Crosspoint app on your X4 first.`, 'error');
    updateSendButton();
    return;
  }

  sendBtn.textContent = '⟳ Sending…';
  sendBtn.disabled = true;

  try {
    const res = await fetch('/api/crosspoint/send', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        ip: defaultCrosspointIp,
        firmware: probe.firmware || 'stock',
        date: activeDate,
      }),
    });
    const data = await res.json();
    if (data.success) {
      if (data.already_sent) {
        toast(`Already on device — no need to resend.`, 'info');
      } else {
        toast(`✅ Sent to ${defaultDev.name} successfully!`, 'success');
      }
      sentHistory[histKey] = new Date().toISOString();
    } else {
      toast(`❌ Send failed: ${data.message}`, 'error');
    }
  } catch(e) {
    toast(`❌ Send failed: ${e.message}`, 'error');
  }
  updateSendButton();
});
