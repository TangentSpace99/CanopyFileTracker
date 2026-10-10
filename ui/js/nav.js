// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

// Choosing a root and moving between folders: breadcrumbs, status bar and the
// comparison baseline.

import { ROOT_ID, S, activeRoot, val } from './state.js';
import { esc, fmtSize, fmtCount, fmtMs, fmtWhen, fmtAgo } from './format.js';
import { call } from './api.js';
import { $, setMode } from './ui.js';
import { renderSidebar } from './sidebar.js';
import { startScan, showScanning, liveTick } from './scan.js';
import { loadTree, renderTree } from './tree.js';
import { drawMap, clearMap } from './treemap.js';
import { resetTabs, refreshTab } from './tabs.js';

/** Makes a root the active one: resets per-root state, then shows its index or starts its first scan. */
export async function selectRoot(key) {
  S.key = key;
  S.cwd = ROOT_ID;
  S.node = null;
  S.rows = [];
  S.sel = null;
  S.baselineAuto = true;
  $('#search').value = '';
  // Nothing from the previous root may linger while the new one loads.
  resetTabs();
  clearMap();
  renderTree();
  try { localStorage.setItem('canopy.root', key); } catch { /* storage unavailable */ }
  renderSidebar();
  const r = activeRoot();
  if (!r) return;
  $('#refresh').classList.toggle('busy', !!(r.scanning && S.scans[key] && !S.scans[key].silent));
  if (!r.complete) {
    showScanning(r);
    if (!r.scanning) await startScan(key, false);
    return;
  }
  await loadSnapshots();
  setMode('browse');
  await navigate(ROOT_ID);
  liveTick();
}

/** Loads the history buckets and fills the "Compare with" dropdown. */
export async function loadSnapshots() {
  S.snapshots = (await call('snapshots', { key: S.key })) || [];
  const ids = S.snapshots.map((s) => s.id);
  if (S.baselineAuto || (S.baseline >= 0 && !ids.includes(S.baseline))) {
    S.baseline = ids.length ? ids[ids.length - 1] : -1;   // oldest bucket: everything since the first index
  }
  const sel = $('#baseline');
  const opts = ['<option value="-1">nothing</option>'];
  S.snapshots.forEach((s, i) => {
    const oldest = i === S.snapshots.length - 1;
    opts.push(`<option value="${s.id}">${oldest ? 'first index · ' : ''}${fmtWhen(s.ts)}</option>`);
  });
  sel.innerHTML = opts.join('');
  sel.value = String(S.baseline);
}

/**
 * Shows folder `id`. `keep` preserves expansion, selection and scroll position;
 * `quiet` marks a background refresh, which must not disturb what is on screen.
 */
export async function navigate(id, keep = false, quiet = false) {
  const token = ++S.nav;
  const node = await call('node', { key: S.key, id, baseline: S.baseline });
  if (token !== S.nav) return;
  if (!node || !node.row.is_dir) {
    if (id !== ROOT_ID) return navigate(node ? node.row.parent : ROOT_ID, false);
    return;
  }
  const moved = S.cwd !== id;
  S.cwd = id;
  S.node = node;
  renderCrumbs();
  renderStatus();
  await loadTree(keep && !moved, token);
  if (token !== S.nav) return;
  drawMap(quiet);
  refreshTab(quiet);
}

/** Draws the breadcrumb trail of the current folder. */
function renderCrumbs() {
  const crumbs = S.node.crumbs;
  $('#crumbs').innerHTML = crumbs
    .map((c, i) => `<span class="crumb" data-id="${c.id}" title="${esc(c.name)}">${esc(c.name)}</span>${i < crumbs.length - 1 ? '<span class="sep">›</span>' : ''}`)
    .join('');
  $('#up').disabled = S.cwd === ROOT_ID;
}

/** Draws the status bar: numbers of the current folder on the left, freshness of the index on the right. */
export function renderStatus() {
  const r = activeRoot();
  if (!r || !S.node) { $('#status-left').innerHTML = ''; $('#status-right').innerHTML = ''; return; }
  const n = S.node.row;
  const other = S.metric === 'alloc' ? `${fmtSize(n.size)} logical` : `${fmtSize(n.alloc)} on disk`;
  $('#status-left').innerHTML = `<b>${fmtSize(val(n))}</b> · ${other} · ${fmtCount(n.files)} files · ${fmtCount(n.dirs)} folders`;
  const last = r.last_mode === 'journal' ? `last diff ${fmtMs(r.last_ms)}` : `full scan took ${fmtMs(r.last_ms)}`;
  const how = r.journal
    ? `<span class="bolt">Live</span> · ${last} · checked ${fmtAgo(r.last_scan)}`
    : `No change journal on this volume · ${last} · ${fmtAgo(r.last_scan)}`;
  const skipped = r.last_errors > 0 ? ` · ${fmtCount(r.last_errors)} protected folders skipped` : '';
  $('#status-right').innerHTML = how + skipped;
}
