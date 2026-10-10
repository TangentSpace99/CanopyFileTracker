// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

// The list views behind the tabs: largest files, file types and changes.

import { ROOT_ID, S, val, joinPath } from './state.js';
import { esc, fmtSize, fmtCount, fmtCompact, fmtDate, fmtWhen } from './format.js';
import { extOf, extStyle } from './filetypes.js';
import { invoke, call } from './api.js';
import { $, $$, itemMenu } from './ui.js';
import { navigate } from './nav.js';
import { renderTree, select } from './tree.js';
import { drawMap } from './treemap.js';

/** Shows one of the views and loads its content. */
export function setTab(tab) {
  S.tab = tab;
  for (const b of $$('.tab')) b.classList.toggle('on', b.dataset.tab === tab);
  for (const name of ['folders', 'files', 'types', 'changes', 'search']) $('#view-' + name).hidden = name !== tab;
  refreshTab();
  if (tab === 'folders') { renderTree(); drawMap(); }
}

// Remembers what each list view currently shows, so unchanged views are not reloaded.
const tabCache = {};

/** Reloads the visible list view after navigation or after the index changed. */
export function refreshTab(quiet = false) {
  if (!S.node) return;
  if (S.tab === 'files') showFiles();
  else if (S.tab === 'types') showTypes(quiet);
  else if (S.tab === 'changes') showChanges();
  updateChangesBadge();
}

/** Returns to the Folders tab and forgets everything the list views were showing. */
export function resetTabs() {
  S.tab = 'folders';
  for (const b of $$('.tab')) b.classList.toggle('on', b.dataset.tab === 'folders');
  for (const name of ['folders', 'files', 'types', 'changes', 'search']) {
    const view = $('#view-' + name);
    view.hidden = name !== 'folders';
    if (name !== 'folders') view.innerHTML = '';
  }
  for (const k of Object.keys(tabCache)) delete tabCache[k];
  $('#changes-badge').hidden = true;
}

/** Name of the current folder, for headings. */
const scopeLabel = () => (S.cwd === ROOT_ID ? S.node.path : S.node.row.name);

/** Table rows for a list of files or folders. */
export function hitRows(hits) {
  return hits.map((f, i) => {
    const icon = f.is_dir
      ? `<span class="chip" style="background:#e3b341"></span>`
      : `<span class="chip" style="background:${extStyle(extOf(f.name)).css}"></span>`;
    return `<tr data-i="${i}">
      <td>${icon}<span class="name">${esc(f.name)}</span><span class="where">${esc(f.dir)}</span></td>
      <td class="num"><b>${fmtSize(val(f))}</b></td>
      <td class="num">${fmtDate(f.mtime)}</td>
    </tr>`;
  }).join('');
}

/** Click and context-menu handling for such a table. A click jumps to the item in the tree. */
export function wireHits(view, hits) {
  const pick = (e) => { const tr = e.target.closest('tr[data-i]'); return tr ? hits[Number(tr.dataset.i)] : null; };
  view.onclick = (e) => {
    const f = pick(e);
    if (!f) return;
    $('#search').value = '';
    setTab('folders');
    navigate(f.is_dir ? f.id : f.parent).then(() => {
      const row = S.rows.find((r) => r.id === f.id);
      if (row) select(row, true);
    });
  };
  view.oncontextmenu = (e) => {
    const f = pick(e);
    if (f) itemMenu(e.clientX, e.clientY, joinPath(f.dir, f.name), f.is_dir, f.is_dir ? f.id : f.parent);
  };
}

export const HIT_HEAD = '<table><colgroup><col><col style="width:120px"><col style="width:130px"></colgroup><thead><tr><th>Name</th><th class="num">Size</th><th class="num">Modified</th></tr></thead><tbody>';

/** The "Largest files" view of the current folder. */
async function showFiles() {
  const view = $('#view-files');
  const scope = `${S.key}:${S.cwd}`;
  if (tabCache.files !== scope) view.innerHTML = '<div class="empty">Looking for the largest files…</div>';
  const hits = await call('top_files', { key: S.key, id: S.cwd, limit: 300 });
  if (!hits || S.tab !== 'files' || scope !== `${S.key}:${S.cwd}`) return;
  tabCache.files = scope;
  view.innerHTML = hits.length
    ? `<div class="list-title"><h3>Largest files</h3><span>in ${esc(scopeLabel())} · click to locate</span></div>${HIT_HEAD}${hitRows(hits)}</tbody></table>`
    : '<div class="empty"><b>No files here</b>This folder contains no files.</div>';
  wireHits(view, hits);
}

/** The "File types" view of the current folder. */
async function showTypes(quiet = false) {
  const view = $('#view-types');
  const scope = `${S.key}:${S.cwd}:${S.metric}`;
  const tag = `${scope}:${S.gen}`;
  if (tabCache.types === tag) return;
  // This is the one expensive view: background refreshes leave it alone, and a
  // recount of the same folder keeps the old figures on screen until it lands.
  if (tabCache.typesScope === scope) { if (quiet) return; }
  else view.innerHTML = '<div class="empty">Summing up file types…</div>';
  const types = await call('file_types', { key: S.key, id: S.cwd });
  if (!types || S.tab !== 'types' || scope !== `${S.key}:${S.cwd}:${S.metric}`) return;
  tabCache.types = tag;
  tabCache.typesScope = scope;
  types.sort((a, b) => val(b) - val(a));
  const total = types.reduce((sum, t) => sum + val(t), 0) || 1;
  if (!types.length) { view.innerHTML = '<div class="empty"><b>No files here</b>This folder contains no files.</div>'; return; }

  const top = types.slice(0, 14);
  const rest = total - top.reduce((sum, t) => sum + val(t), 0);
  const stack = top.map((t) => `<i style="flex:${val(t)};background:${extStyle(t.ext).css}" title=".${esc(t.ext)} · ${fmtSize(val(t))}"></i>`).join('')
    + (rest > 0 ? `<i style="flex:${rest};background:#2a3340" title="Everything else · ${fmtSize(rest)}"></i>` : '');
  const rows = types.slice(0, 150).map((t) => {
    const st = extStyle(t.ext);
    const pct = (val(t) / total) * 100;
    return `<tr>
      <td><span class="chip" style="background:${st.css}"></span><span class="name">${t.ext ? '.' + esc(t.ext) : 'no extension'}</span><span class="cat">${st.cat}</span></td>
      <td><div class="c-share"><div class="bar"><i style="width:${pct.toFixed(1)}%;background:${st.css}"></i></div><span class="pct">${pct.toFixed(1)}%</span></div></td>
      <td class="num"><b>${fmtSize(val(t))}</b></td>
      <td class="num">${fmtCount(t.count)}</td>
    </tr>`;
  }).join('');
  view.innerHTML = `<div class="list-title"><h3>File types</h3><span>in ${esc(scopeLabel())} · ${fmtCount(types.length)} types · colours match the treemap</span></div>
    <div class="stack">${stack}</div>
    <table><colgroup><col><col style="width:34%"><col style="width:120px"><col style="width:120px"></colgroup>
    <thead><tr><th>Type</th><th>Share</th><th class="num">Size</th><th class="num">Files</th></tr></thead><tbody>${rows}</tbody></table>`;
  view.querySelector('tbody').style.cursor = 'default';
}

/** The "Changes" view: what was added, removed or resized below the current folder since the baseline. */
async function showChanges() {
  const view = $('#view-changes');
  if (S.baseline < 0) {
    view.innerHTML = '<div class="empty"><b>Comparison is off</b>Choose a point in time under “Compare with” to see what changed since then.</div>';
    return;
  }
  const prefix = S.cwd === ROOT_ID ? '' : S.node.path;
  const res = await call('changes', { key: S.key, baseline: S.baseline, prefix, limit: 400 });
  if (!res || S.tab !== 'changes') return;
  const snap = S.snapshots.find((s) => s.id === S.baseline);
  const since = snap ? fmtWhen(snap.ts) : '';
  if (!res.total) {
    view.innerHTML = `<div class="empty"><b>Nothing has changed here since ${esc(since)}</b>Canopy records every file that appears, disappears or changes size.<br>Leave it open, or come back later and press Refresh.</div>`;
    return;
  }
  const net = res.grown - res.shrunk;
  const labels = { 1: ['+', 'added'], 2: ['−', 'removed'], 3: ['~', 'resized'] };
  const rows = res.events.map((ev, i) => {
    const cut = ev.path.lastIndexOf('\\');
    const d = ev.new_size - ev.old_size;
    const [sign, word] = labels[ev.kind] || ['?', ''];
    return `<tr data-i="${i}">
      <td><span class="sign k${ev.kind}">${sign}</span><span class="name">${esc(ev.path.slice(cut + 1))}</span><span class="where">${esc(ev.path.slice(0, cut))}</span></td>
      <td class="num" style="color:var(--faint)">${ev.is_dir ? 'folder ' : ''}${word}</td>
      <td class="num"><b class="${d > 0 ? 'up' : 'down'}">${d > 0 ? '+' : ''}${fmtSize(d)}</b></td>
    </tr>`;
  }).join('');
  view.innerHTML = `<div class="list-title"><h3>Changes</h3><span>in ${esc(scopeLabel())} since ${esc(since)}</span></div>
    <div class="cards">
      <div class="card"><span>Net change</span><b class="${net > 0 ? 'up' : net < 0 ? 'down' : ''}">${net > 0 ? '+' : ''}${fmtSize(net)}</b></div>
      <div class="card"><span>Grew by</span><b>${fmtSize(res.grown)}</b></div>
      <div class="card"><span>Freed</span><b>${fmtSize(res.shrunk)}</b></div>
      <div class="card"><span>Items changed</span><b>${fmtCount(res.total)}</b></div>
    </div>
    <table><colgroup><col><col style="width:140px"><col style="width:130px"></colgroup>
    <thead><tr><th>Item${res.total > res.events.length ? ` (largest ${res.events.length} of ${fmtCount(res.total)})` : ''}</th><th class="num">What</th><th class="num">Change</th></tr></thead><tbody>${rows}</tbody></table>`;
  view.oncontextmenu = (e) => {
    const tr = e.target.closest('tr[data-i]');
    const ev = tr && res.events[Number(tr.dataset.i)];
    if (ev && ev.kind !== 2) itemMenu(e.clientX, e.clientY, ev.path, ev.is_dir, 0);
  };
  view.onclick = (e) => {
    const tr = e.target.closest('tr[data-i]');
    const ev = tr && res.events[Number(tr.dataset.i)];
    if (ev && ev.kind !== 2) call('reveal', { path: ev.path });
  };
}

/** Shows the number of changes in the whole root next to the tab title. */
async function updateChangesBadge() {
  const badge = $('#changes-badge');
  if (S.baseline < 0 || !S.key) { badge.hidden = true; return; }
  const res = await invoke('changes', { key: S.key, baseline: S.baseline, prefix: '', limit: 0 }).catch(() => null);
  badge.hidden = !res || !res.total;
  if (res && res.total) badge.textContent = fmtCompact(res.total);
}
