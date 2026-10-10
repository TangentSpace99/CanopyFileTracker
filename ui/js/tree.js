// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

// The folder tree: a virtual list of expandable rows with selection, sorting
// and keyboard control.

import { PAGE, ROW_H, ATTR_REPARSE, S, val, oldVal, joinPath } from './state.js';
import { esc, fmtSize, fmtCount, fmtDate } from './format.js';
import { extOf, extStyle } from './filetypes.js';
import { invoke, call } from './api.js';
import { $, $$, toast, itemMenu, FOLDER_ICON, CHEVRON } from './ui.js';
import { navigate } from './nav.js';
import { drawHighlight } from './treemap.js';

// A tree row is { id, r, depth, parentRow, open }: `r` is the row from the
// backend, `depth` the indentation level, `parentRow` the row it was expanded
// from (null at the top level). A "more" row is a placeholder that loads the
// next page of a long folder when clicked.
const mkRow = (r, depth, parentRow) => ({ id: r.id, r, depth, parentRow, open: false });
const moreRow = (depth, parentRow, offset) => ({ more: true, depth, parentRow, offset });

/** Requests one page of a folder's children in the current sort order. */
function fetchChildren(parent, offset) {
  const sort = S.sort === 'size' && S.metric === 'alloc' ? 'alloc' : S.sort;
  return invoke('children', { key: S.key, parent, sort, desc: S.desc, offset, limit: PAGE, baseline: S.baseline });
}

/** Wraps backend rows as tree rows and appends a "more" row when the page was full. */
function toRows(kids, depth, parentRow, offset) {
  const rows = kids.map((r) => mkRow(r, depth, parentRow));
  if (kids.length === PAGE) rows.push(moreRow(depth, parentRow, offset + PAGE));
  return rows;
}

/**
 * Loads the top level of the tree for the current folder. With `keep`, folders
 * that were expanded are expanded again and selection and scroll position
 * survive. That is what makes a background refresh invisible.
 */
export async function loadTree(keep, token) {
  const scroller = $('#tree-scroll');
  const open = keep ? new Set(S.rows.filter((r) => r.open).map((r) => r.id)) : new Set();
  const selId = keep && S.sel ? S.sel.id : null;
  const top = keep ? scroller.scrollTop : 0;
  let kids;
  try { kids = await fetchChildren(S.cwd, 0); } catch (e) { toast(`<b>Error</b> ${esc(e)}`, true); return; }
  if (token !== S.nav) return;
  const rows = toRows(kids, 0, null, 0);
  for (let i = 0; i < rows.length && open.size; i++) {
    const row = rows[i];
    if (row.more || !row.r.is_dir || !open.has(row.id)) continue;
    const sub = await fetchChildren(row.id, 0).catch(() => []);
    if (token !== S.nav) return;
    row.open = true;
    rows.splice(i + 1, 0, ...toRows(sub, row.depth + 1, row, 0));
  }
  S.rows = rows;
  S.sel = selId ? rows.find((r) => r.id === selId) || null : null;
  renderHead();
  renderTree();
  scroller.scrollTop = top;
}

/** Marks the sorted column in the table header. */
function renderHead() {
  for (const el of $$('#tree-head > div')) {
    // Two columns sort by size; only the numeric one carries the arrow.
    const on = el.dataset.sort === S.sort && (el.dataset.sort !== 'size' || el.classList.contains('num'));
    el.classList.toggle('sorted', on);
    el.classList.toggle('asc', !S.desc);
  }
}

/** Full path of a tree row, built from the names of its ancestors. */
function rowPath(row) {
  const parts = [];
  for (let r = row; r; r = r.parentRow) parts.push(r.r.name);
  return parts.reverse().reduce(joinPath, S.node.path);
}

/** HTML of one tree row. `i` is its index in `S.rows`; it is stored on the element so clicks can be traced back. */
function rowHtml(row, i) {
  const pad = 8 + row.depth * 18;
  if (row.more) {
    return `<div class="row grid more" data-i="${i}"><div class="c-name" style="padding-left:${pad}px"><span class="chev none"></span><span class="nm">Show more…</span></div><div></div><div></div><div></div><div></div><div></div></div>`;
  }
  const r = row.r;
  const v = val(r);
  const base = row.parentRow ? val(row.parentRow.r) : val(S.node.row);
  const pct = base > 0 ? Math.min(100, (v / base) * 100) : 0;
  const link = r.is_dir && (r.attrs & ATTR_REPARSE);
  const expandable = r.is_dir && !link && (r.files + r.dirs > 0);
  const icon = r.is_dir
    ? `<span class="ico" style="color:${link ? '#5f6b7d' : '#e3b341'}">${FOLDER_ICON}</span>`
    : `<span class="ico file" style="--c:${extStyle(extOf(r.name)).css}"><i></i></span>`;
  let delta = '';
  const old = oldVal(r);
  if (r.is_dir && old != null && old !== v) {
    const d = v - old;
    delta = `<span class="${d > 0 ? 'up' : 'down'}">${d > 0 ? '+' : ''}${fmtSize(d)}</span>`;
  }
  const cls = 'row grid' + (row === S.sel ? ' sel' : '') + (link ? ' link' : '');
  return `<div class="${cls}" data-i="${i}">
    <div class="c-name" style="padding-left:${pad}px">
      <span class="chev${expandable ? (row.open ? ' open' : '') : ' none'}">${CHEVRON}</span>${icon}<span class="nm">${esc(r.name)}</span>
    </div>
    <div class="c-share"><div class="bar"><i class="${pct > 66 ? 'hot' : pct > 33 ? 'mid' : ''}" style="width:${pct.toFixed(1)}%"></i></div><span class="pct">${pct >= 99.95 ? '100' : pct.toFixed(1)}%</span></div>
    <div class="c-size num">${fmtSize(v)}</div>
    <div class="num dim">${r.is_dir ? fmtCount(r.files) : ''}</div>
    <div class="num dim">${fmtDate(r.mtime)}</div>
    <div class="num">${delta}</div>
  </div>`;
}

// Virtual list: only the rows in view (plus a small margin) exist in the DOM.
// A spacer element gives the scrollbar the full height, and the rendered slice
// is moved into place with a transform. Folders with 100,000 entries stay smooth.
// `treeFrame` is the pending animation frame: many scroll events, one render.
let treeFrame = 0;

/** Renders the visible slice of `S.rows`. */
export function renderTree() {
  treeFrame = 0;
  const scroller = $('#tree-scroll');
  const start = Math.max(0, Math.floor(scroller.scrollTop / ROW_H) - 8);
  const end = Math.min(S.rows.length, Math.ceil((scroller.scrollTop + scroller.clientHeight) / ROW_H) + 8);
  $('#tree-spacer').style.height = S.rows.length * ROW_H + 'px';
  const el = $('#tree-rows');
  el.style.transform = `translateY(${start * ROW_H}px)`;
  let html = '';
  for (let i = start; i < end; i++) html += rowHtml(S.rows[i], i);
  el.innerHTML = html || (S.node ? '<div class="empty">This folder is empty.</div>' : '');
}

const scheduleTree = () => { if (!treeFrame) treeFrame = requestAnimationFrame(renderTree); };

/** Removes every row below `row` that is nested deeper than it. */
function collapse(row) {
  row.open = false;
  const i = S.rows.indexOf(row);
  let j = i + 1;
  while (j < S.rows.length && S.rows[j].depth > row.depth) j++;
  S.rows.splice(i + 1, j - i - 1);
}

/** Loads the children of `row` and inserts them right after it. */
async function expand(row) {
  if (row.open || row.loading) return;
  row.loading = true;
  const token = S.nav;
  const kids = await fetchChildren(row.id, 0).catch(() => null);
  row.loading = false;
  const i = S.rows.indexOf(row);
  if (!kids || token !== S.nav || i < 0) return;
  row.open = true;
  S.rows.splice(i + 1, 0, ...toRows(kids, row.depth + 1, row, 0));
}

/** Expands or collapses a folder row. */
export async function toggle(row) {
  if (row.more || !row.r.is_dir) return;
  if (row.open) collapse(row); else await expand(row);
  renderTree();
}

/** Replaces a "more" row with the next page of entries. */
async function loadMore(row) {
  const parent = row.parentRow ? row.parentRow.id : S.cwd;
  const token = S.nav;
  const kids = await fetchChildren(parent, row.offset).catch(() => null);
  const i = S.rows.indexOf(row);
  if (!kids || token !== S.nav || i < 0) return;
  S.rows.splice(i, 1, ...toRows(kids, row.depth, row.parentRow, row.offset));
  renderTree();
}

/**
 * Selects a row. Only the CSS class is moved and the list is not rendered
 * again: replacing the elements between two clicks would break double-click.
 */
export function select(row, scroll = false) {
  S.sel = row;
  const i = S.rows.indexOf(row);
  if (scroll && i >= 0) {
    const scroller = $('#tree-scroll');
    const y = i * ROW_H;
    if (y < scroller.scrollTop) scroller.scrollTop = y;
    else if (y + ROW_H > scroller.scrollTop + scroller.clientHeight) scroller.scrollTop = y + ROW_H - scroller.clientHeight;
  }
  for (const el of $$('#tree-rows .row.sel')) el.classList.remove('sel');
  const el = $(`#tree-rows .row[data-i="${i}"]`);
  if (el) el.classList.add('sel');
  drawHighlight();
}

/** The tree row on which an event happened. */
function rowFromEvent(e) {
  const el = e.target.closest('.row');
  return el ? S.rows[Number(el.dataset.i)] : null;
}

/**
 * Attaches the event handlers of the tree. They sit on the container and not
 * on the rows, because rows are recreated on every render.
 */
export function wireTree() {
  const rowsEl = $('#tree-rows');
  const scroller = $('#tree-scroll');
  scroller.addEventListener('scroll', scheduleTree);
  new ResizeObserver(scheduleTree).observe(scroller);

  rowsEl.addEventListener('click', (e) => {
    const row = rowFromEvent(e);
    if (!row) return;
    if (row.more) return loadMore(row);
    select(row);
    if (e.target.closest('.chev')) toggle(row);
  });
  rowsEl.addEventListener('dblclick', (e) => {
    const row = rowFromEvent(e);
    if (!row || row.more || e.target.closest('.chev')) return;
    if (row.r.is_dir) navigate(row.id);
    else call('reveal', { path: rowPath(row) });
  });
  rowsEl.addEventListener('contextmenu', (e) => {
    const row = rowFromEvent(e);
    if (!row || row.more) return;
    select(row);
    itemMenu(e.clientX, e.clientY, rowPath(row), row.r.is_dir, row.r.is_dir ? row.id : 0);
  });

  $('#tree-head').addEventListener('click', (e) => {
    const key = e.target.dataset.sort;
    if (!key) return;
    if (S.sort === key) S.desc = !S.desc;
    else { S.sort = key; S.desc = key !== 'name'; }
    loadTree(false, S.nav);
  });

  scroller.addEventListener('keydown', async (e) => {
    const i = S.sel ? S.rows.indexOf(S.sel) : -1;
    const row = S.sel;
    let handled = true;
    if (e.key === 'ArrowDown') { if (i < S.rows.length - 1) select(S.rows[i + 1], true); }
    else if (e.key === 'ArrowUp') { if (i > 0) select(S.rows[i - 1], true); }
    else if (e.key === 'ArrowRight' && row && !row.more) {
      if (row.r.is_dir && !row.open) await toggle(row);
      else if (i < S.rows.length - 1) select(S.rows[i + 1], true);
    } else if (e.key === 'ArrowLeft' && row && !row.more) {
      if (row.open) await toggle(row);
      else if (row.parentRow) select(row.parentRow, true);
    } else if (e.key === 'Enter' && row && !row.more) {
      if (row.r.is_dir) navigate(row.id);
      else call('reveal', { path: rowPath(row) });
    } else handled = false;
    if (handled) e.preventDefault();
  });
}
