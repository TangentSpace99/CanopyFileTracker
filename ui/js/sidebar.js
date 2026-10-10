// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

// The sidebar: indexed roots, the drives of this PC, adding and removing roots.

import { S, val } from './state.js';
import { esc, fmtSize, fmtCompact, fmtAgo } from './format.js';
import { invoke, call } from './api.js';
import { $, setMode, DRIVE_ICON, FOLDER_ICON } from './ui.js';
import { selectRoot } from './nav.js';

/** Second line of a root in the sidebar: scan progress, or size and live status. */
function rootSubHtml(r) {
  const scan = S.scans[r.key];
  if (r.scanning && scan && !scan.silent) {
    return `<span class="dots"></span><span id="sub-${r.key}">${r.complete ? 'Rescanning…' : 'Indexing…'}</span>`;
  }
  if (!r.complete) return '<span>Not indexed yet</span>';
  const live = r.journal
    ? '<span class="live" title="Kept current from the NTFS change journal">live</span>'
    : `<span class="live off" title="No change journal on this volume: refreshes are full rescans">${fmtAgo(r.last_scan)}</span>`;
  return `<span>${fmtSize(val(r))} · ${fmtCompact(r.files)} files</span>${live}`;
}

/** Redraws the whole sidebar from `S.roots` and `S.drives`. */
export function renderSidebar() {
  const rootsEl = $('#roots');
  $('#roots-section').hidden = S.roots.length === 0;
  rootsEl.innerHTML = S.roots.map((r) => {
    const isVolume = r.path.length <= 3;
    // The meter shows how much of the volume this root occupies (green) and how
    // much of the volume is used by everything else (grey).
    const share = r.vol_total ? Math.min(100, (r.alloc / r.vol_total) * 100) : 0;
    const used = r.vol_total ? ((r.vol_total - r.vol_free) / r.vol_total) * 100 : 0;
    const rest = Math.max(0, used - share);
    return `<div class="root${r.key === S.key ? ' active' : ''}" data-key="${r.key}" title="${esc(r.path)}">
      <div class="root-top">${isVolume ? DRIVE_ICON : FOLDER_ICON}
        <div class="root-name">${esc(r.path)}${isVolume && r.label ? `<small>${esc(r.label)}</small>` : ''}</div>
        <button class="root-x" title="Remove this index">×</button>
      </div>
      <div class="meter" title="${share.toFixed(1)}% of the volume is in this root; ${used.toFixed(0)}% of the volume is in use">
        <i style="width:${share}%"></i>${rest > 0.5 ? `<i class="rest" style="width:${rest}%"></i>` : ''}
      </div>
      <div class="root-sub">${rootSubHtml(r)}</div>
    </div>`;
  }).join('');

  const indexed = new Set(S.roots.map((r) => r.path.toLowerCase()));
  const kinds = { 2: 'Removable', 4: 'Network', 5: 'Optical', 6: 'RAM disk' };
  $('#drives').innerHTML = S.drives.filter((d) => !indexed.has(d.path.toLowerCase())).map((d) => {
    const used = d.total - d.free;
    const pct = d.total ? (used / d.total) * 100 : 0;
    const tag = [d.label, kinds[d.kind], d.fs].filter(Boolean).join(' · ');
    return `<div class="drive" data-path="${esc(d.path)}">
      <div class="root-top">${DRIVE_ICON}<div class="root-name">${esc(d.path)}<small>${esc(tag)}</small></div></div>
      <div class="meter${pct > 90 ? ' hot' : ''}"><i style="width:${pct}%"></i></div>
      <div class="root-sub"><span>${fmtSize(used)} of ${fmtSize(d.total)}</span><button class="mini">Index</button></div>
    </div>`;
  }).join('') || '<div class="root-sub" style="padding:4px 18px">Every drive is indexed.</div>';

  $('#side-foot').innerHTML = S.elevated
    ? 'Running as administrator: protected folders are included.'
    : 'Protected system folders are skipped. <a id="elevate">Restart as administrator</a> to include them.';
}

/** Draws the drive cards of the start screen. */
function renderWelcome() {
  $('#welcome-drives').innerHTML = S.drives.map((d) => {
    const used = d.total - d.free;
    const pct = d.total ? (used / d.total) * 100 : 0;
    return `<div class="dcard" data-path="${esc(d.path)}">
      <b>${esc(d.path)}</b><small>${esc(d.label || d.fs)}</small>
      <div class="meter${pct > 90 ? ' hot' : ''}"><i style="width:${pct}%"></i></div>
      <div class="root-sub"><span>${fmtSize(used)} used of ${fmtSize(d.total)}</span></div>
    </div>`;
  }).join('');
}

/** Reloads the list of roots from the backend. */
export async function refreshRoots() {
  const roots = await call('list_roots');
  if (roots) S.roots = roots;
  renderSidebar();
}

/** Re-reads one root's numbers without probing every other volume. */
export async function refreshRoot(key) {
  const info = await invoke('root_overview', { key }).catch(() => null);
  const i = S.roots.findIndex((r) => r.key === key);
  if (info && i >= 0) S.roots[i] = info;
  renderSidebar();
}

/** Reloads the drive list. Slow for disconnected network drives, so nothing waits for it. */
export async function loadDrives() {
  const drives = await call('list_drives');
  if (drives) S.drives = drives;
  renderSidebar();
  renderWelcome();
}

/** Registers a folder as a new root and switches to it, which starts its first scan. */
export async function addRoot(path) {
  const info = await call('add_root', { path });
  if (!info) return;
  await refreshRoots();
  await selectRoot(info.key);
}

/** Deletes a root's index after asking for confirmation. */
export async function removeRoot(key) {
  const r = S.roots.find((x) => x.key === key);
  if (!r) return;
  if (!confirm(`Remove the index for ${r.path}?\n\nOnly Canopy's index is deleted. Your files are not touched.`)) return;
  if ((await call('remove_root', { key })) === null) return;
  if (S.key === key) S.key = null;
  await refreshRoots();
  if (!S.key) {
    if (S.roots.length) await selectRoot(S.roots[0].key);
    else setMode('welcome');
  }
}
