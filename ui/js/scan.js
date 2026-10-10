// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

// Starting scans and reacting to their progress and completion, including the
// automatic refresh of live roots.

import { ROOT_ID, S, activeRoot } from './state.js';
import { esc, fmtSize, fmtCount, fmtCompact, fmtMs } from './format.js';
import { call } from './api.js';
import { $, toast, setMode } from './ui.js';
import { renderSidebar, refreshRoots, refreshRoot } from './sidebar.js';
import { loadSnapshots, navigate, renderStatus } from './nav.js';

/**
 * Starts a refresh of a root. `full` forces a complete walk; `silent` marks a
 * background refresh, which shows neither a spinner nor a toast. The outcome
 * arrives later through the `scan-done` event (see onDone).
 */
export async function startScan(key, full, silent = false) {
  if (S.scans[key]) return;
  const r = S.roots.find((x) => x.key === key);
  S.scans[key] = { silent, t0: performance.now(), first: !(r && r.complete) };
  const started = await call('start_scan', { key, full });
  if (!started) { delete S.scans[key]; return; }
  if (r) r.scanning = true;
  if (!silent) {
    renderSidebar();
    if (key === S.key) $('#refresh').classList.add('busy');
  }
}

/** Shows the first-index screen with all counters at zero. */
export function showScanning(r) {
  setMode('scanning');
  $('#scan-title').textContent = 'Indexing ' + r.path;
  for (const id of ['#scan-files', '#scan-dirs', '#scan-rate']) $(id).textContent = '0';
  $('#scan-bytes').textContent = '0 B';
  $('#scan-path').textContent = '';
}

/** Handles `scan-progress`: updates the first-index screen, or the Refresh button during a rescan. */
export function onProgress(p) {
  const scan = S.scans[p.key];
  if (!scan || scan.silent) return;
  const sub = document.getElementById('sub-' + p.key);
  const phase = { aggregate: 'Summing folders…', index: 'Building index…' }[p.phase];
  if (sub) sub.textContent = phase || `${fmtCompact(p.files)} files…`;
  if (p.key !== S.key) return;
  if (scan.first) {
    const secs = Math.max(0.05, (performance.now() - scan.t0) / 1000);
    $('#scan-files').textContent = fmtCount(p.files);
    $('#scan-dirs').textContent = fmtCount(p.dirs);
    $('#scan-bytes').textContent = fmtSize(p.bytes);
    $('#scan-rate').textContent = fmtCount(Math.round(p.files / secs));
    // U+200E keeps the right-to-left ellipsis trick from reordering the path.
    $('#scan-path').textContent = phase || '‎' + p.current;
  } else {
    $('#refresh-label').textContent = phase || fmtCompact(p.files) + ' files';
  }
}

/** Handles `scan-done`: reports the outcome and reloads what is on screen if the index changed. */
export async function onDone(d) {
  const scan = S.scans[d.key] || { silent: true };
  delete S.scans[d.key];
  const r = S.roots.find((x) => x.key === d.key);
  if (r) r.scanning = false;
  const mine = d.key === S.key;
  if (mine) { $('#refresh').classList.remove('busy'); $('#refresh-label').textContent = 'Refresh'; }

  if (!d.ok) {
    if (!d.cancelled && !scan.silent) toast(`<b>Scan failed</b> ${esc(d.error)}`, true, 8000);
    await refreshRoots();
    if (mine && scan.first) setMode('welcome');
    return;
  }

  const s = d.summary;
  if (r) Object.assign(r, { last_mode: s.mode, last_ms: s.ms, last_scan: Math.floor(Date.now() / 1000) });
  // `S.gen` identifies the content of the index. Cached views compare against it
  // to find out whether they are stale.
  const changed = s.first || s.changed > 0;
  if (changed) S.gen++;

  if (!scan.silent || changed) await refreshRoot(d.key);
  if (!scan.silent) {
    const how = s.mode === 'journal' ? 'change journal' : 'full scan';
    if (s.first) toast(`<b>Indexed</b> ${fmtCount(s.files)} files (${fmtSize(s.size)}) in ${fmtMs(s.ms)}`, false, 7000);
    else if (s.changed) toast(`<b>${fmtCount(s.changed)} change${s.changed === 1 ? '' : 's'}</b> picked up in ${fmtMs(s.ms)} · ${how}`);
    else toast(`<b>Up to date</b> checked in ${fmtMs(s.ms)} · ${how}`);
  }
  if (!mine) return;

  if (scan.first || $('#browse').hidden) {
    await loadSnapshots();
    setMode('browse');
    await navigate(ROOT_ID);
  } else if (changed) {
    await loadSnapshots();
    await navigate(S.cwd, true, scan.silent);
  }
  renderStatus();
}

/** Live roots refresh themselves: a journal diff costs a few milliseconds. */
export function liveTick() {
  const r = activeRoot();
  if (!r || document.hidden || !r.complete || !r.journal || r.scanning) return;
  startScan(r.key, false, true);
}
