// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

// The treemap: painting the layout computed by the backend, hover and click
// handling, and the splitter between tree and map.

import { S, val, joinPath } from './state.js';
import { esc, fmtSize } from './format.js';
import { extOf, extStyle } from './filetypes.js';
import { invoke } from './api.js';
import { $, itemMenu } from './ui.js';
import { navigate } from './nav.js';
import { select } from './tree.js';

// Treemap state.
//   mapRects  the layout from the backend, parents before their children
//   mapToken  discards answers that arrive after a newer request
//   mapHover  index of the rectangle under the mouse, or -1
//   mapSize   canvas size in CSS pixels
let mapRects = [];
let mapToken = 0;
let mapHover = -1;
let mapSize = [0, 0];

/** Shortens `text` with an ellipsis until it fits into `max` pixels. Binary search, because measuring text is slow. */
function fit(ctx, text, max) {
  if (ctx.measureText(text).width <= max) return text;
  let lo = 0, hi = text.length;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (ctx.measureText(text.slice(0, mid) + '…').width <= max) lo = mid; else hi = mid - 1;
  }
  return lo > 0 ? text.slice(0, lo) + '…' : '';
}

/** Sizes a canvas for the pixel density of the screen and returns a context that draws in CSS pixels. */
function sizeCanvas(canvas, w, h) {
  const dpr = window.devicePixelRatio || 1;
  canvas.width = Math.round(w * dpr);
  canvas.height = Math.round(h * dpr);
  canvas.style.width = w + 'px';
  canvas.style.height = h + 'px';
  const ctx = canvas.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  return ctx;
}

/** Requests a fresh layout for the current folder and paints it. The layout itself is computed in Rust. */
export async function drawMap(quiet = false) {
  const wrap = $('#map-wrap');
  const w = wrap.clientWidth, h = wrap.clientHeight;
  if (!S.node || S.tab !== 'folders' || w < 20 || h < 20) return;
  const token = ++mapToken;
  const rects = await invoke('treemap', { key: S.key, id: S.cwd, width: w, height: h, alloc: S.metric === 'alloc' }).catch(() => null);
  if (token !== mapToken || !rects) return;
  mapRects = rects;
  mapSize = [w, h];
  if (!quiet) mapHover = -1;
  else if (mapHover >= rects.length) mapHover = -1;
  if (mapHover < 0) $('#tip').hidden = true;
  paintMap();
  drawHighlight();
}

/**
 * Paints `mapRects`. A folder with rectangles inside it becomes a frame with a
 * title; everything else becomes a tile coloured by file type.
 */
function paintMap() {
  const [w, h] = mapSize;
  const ctx = sizeCanvas($('#map'), w, h);
  ctx.fillStyle = '#0a0d11';
  ctx.fillRect(0, 0, w, h);
  $('#map-empty').hidden = mapRects.length > 0;

  // Mark every rectangle that has children drawn inside it.
  const nested = new Uint8Array(mapRects.length);
  for (const r of mapRects) if (r.p >= 0) nested[r.p] = 1;
  ctx.textBaseline = 'middle';

  for (let i = 0; i < mapRects.length; i++) {
    const r = mapRects[i];
    if (r.d && nested[i]) {
      // A folder drawn as a frame around its contents.
      const shade = 15 + Math.min(r.depth, 6) * 2.4;
      ctx.fillStyle = `hsl(215 18% ${shade}%)`;
      ctx.fillRect(r.x, r.y, r.w, r.h);
      ctx.strokeStyle = 'rgba(0,0,0,.55)';
      ctx.lineWidth = 1;
      ctx.strokeRect(r.x + 0.5, r.y + 0.5, r.w - 1, r.h - 1);
      if (r.w > 54 && r.h > 34) {
        ctx.font = '600 11px "Segoe UI", sans-serif';
        ctx.fillStyle = r.depth === 1 ? '#dbe2ec' : '#a3afc0';
        const label = fit(ctx, r.name, r.w - 12);
        ctx.fillText(label, r.x + 6, r.y + 10.5);
        const used = ctx.measureText(label).width;
        if (r.w - used > 90) {
          ctx.fillStyle = '#69768a';
          ctx.font = '11px "Segoe UI", sans-serif';
          ctx.textAlign = 'right';
          ctx.fillText(fmtSize(r.v), r.x + r.w - 6, r.y + 10.5);
          ctx.textAlign = 'left';
        }
      }
      continue;
    }
    // A leaf: a file, a folder too small to open up, or the "everything else" block.
    const st = r.id === -1 ? { h: 215, s: 8, l: 30 } : r.d ? { h: 218, s: 13, l: 36 } : extStyle(extOf(r.name));
    const gap = r.w > 3 && r.h > 3 ? 0.5 : 0;
    const x = r.x + gap, y = r.y + gap, rw = r.w - gap * 2, rh = r.h - gap * 2;
    if (rw > 6 && rh > 6) {
      const g = ctx.createLinearGradient(x, y, x + rw, y + rh);
      g.addColorStop(0, `hsl(${st.h} ${st.s}% ${st.l + 9}%)`);
      g.addColorStop(1, `hsl(${st.h} ${st.s}% ${st.l - 9}%)`);
      ctx.fillStyle = g;
    } else {
      ctx.fillStyle = `hsl(${st.h} ${st.s}% ${st.l}%)`;
    }
    ctx.fillRect(x, y, rw, rh);
    if (rw > 58 && rh > 18) {
      ctx.font = '600 11px "Segoe UI", sans-serif';
      ctx.fillStyle = 'rgba(8,12,18,.86)';
      const name = r.id === -1 ? 'smaller items' : r.name;
      const two = rh > 34;
      ctx.fillText(fit(ctx, name, rw - 10), x + 5, y + (two ? rh / 2 - 7 : rh / 2));
      if (two) {
        ctx.font = '11px "Segoe UI", sans-serif';
        ctx.fillStyle = 'rgba(8,12,18,.66)';
        ctx.fillText(fit(ctx, fmtSize(r.v), rw - 10), x + 5, y + rh / 2 + 7);
      }
    }
  }
}

/** Empties the map, for the moment between leaving one root and showing the next. */
export function clearMap() {
  mapRects = [];
  mapToken++;
  mapHover = -1;
  if (mapSize[0]) { paintMap(); drawHighlight(); }
  $('#map-empty').hidden = true;
}

/** Index of the innermost rectangle at a point, or -1. */
function mapHit(mx, my) {
  // Later rectangles are deeper, so scanning backwards finds the innermost one.
  for (let i = mapRects.length - 1; i >= 0; i--) {
    const r = mapRects[i];
    if (mx >= r.x && mx < r.x + r.w && my >= r.y && my < r.y + r.h) return i;
  }
  return -1;
}

/** Index of the top-level rectangle that contains rectangle `i`. */
function mapTop(i) {
  while (i >= 0 && mapRects[i].p >= 0) i = mapRects[i].p;
  return i;
}

/** Full path of the item behind rectangle `i`. */
function mapPath(i) {
  const parts = [];
  for (let k = i; k >= 0; k = mapRects[k].p) if (mapRects[k].id !== -1) parts.push(mapRects[k].name);
  return parts.reverse().reduce(joinPath, S.node.path);
}

/**
 * Draws the selection and hover outlines on a second canvas above the map, so
 * that moving the mouse never repaints the map itself.
 */
export function drawHighlight() {
  const [w, h] = mapSize;
  if (!w) return;
  const ctx = sizeCanvas($('#map-hi'), w, h);
  const stroke = (r, color, width) => {
    ctx.strokeStyle = color;
    ctx.lineWidth = width;
    ctx.strokeRect(r.x + width / 2, r.y + width / 2, Math.max(0, r.w - width), Math.max(0, r.h - width));
  };
  if (S.sel && !S.sel.more) {
    const i = mapRects.findIndex((r) => r.id === S.sel.id);
    if (i >= 0) stroke(mapRects[i], '#34d399', 2);
  }
  if (mapHover >= 0) {
    const top = mapTop(mapHover);
    if (top !== mapHover) stroke(mapRects[top], 'rgba(255,255,255,.35)', 1);
    stroke(mapRects[mapHover], '#ffffff', 2);
  }
}

/** Attaches mouse handling, resizing and the splitter of the treemap. */
export function wireMap() {
  const wrap = $('#map-wrap');
  const tip = $('#tip');
  const local = (e) => { const b = wrap.getBoundingClientRect(); return [e.clientX - b.left, e.clientY - b.top]; };

  wrap.addEventListener('mousemove', (e) => {
    const [mx, my] = local(e);
    const i = mapHit(mx, my);
    if (i !== mapHover) { mapHover = i; drawHighlight(); }
    if (i < 0) { tip.hidden = true; return; }
    const r = mapRects[i];
    const total = val(S.node.row) || 1;
    const name = r.id === -1 ? 'Smaller items' : r.name;
    tip.innerHTML = `<b>${esc(name)}</b><span class="p">${esc(mapPath(r.id === -1 ? r.p : i))}</span><span class="s">${fmtSize(r.v)} · ${((r.v / total) * 100).toFixed(1)}% of this view${r.d ? ' · folder' : ''}</span>`;
    tip.hidden = false;
    const b = tip.getBoundingClientRect();
    tip.style.left = Math.min(e.clientX + 16, innerWidth - b.width - 8) + 'px';
    tip.style.top = (e.clientY + 18 + b.height > innerHeight ? e.clientY - b.height - 10 : e.clientY + 18) + 'px';
  });
  wrap.addEventListener('mouseleave', () => { tip.hidden = true; mapHover = -1; drawHighlight(); });

  wrap.addEventListener('click', (e) => {
    const i = mapTop(mapHit(...local(e)));
    if (i < 0) return;
    const row = S.rows.find((r) => !r.more && r.depth === 0 && r.id === mapRects[i].id);
    if (row) select(row, true);
  });
  wrap.addEventListener('dblclick', (e) => {
    // Zoom into the deepest folder under the cursor.
    let i = mapHit(...local(e));
    while (i >= 0 && !(mapRects[i].d && mapRects[i].id !== -1)) i = mapRects[i].p;
    if (i >= 0) { tip.hidden = true; navigate(mapRects[i].id); }
  });
  wrap.addEventListener('contextmenu', (e) => {
    const i = mapHit(...local(e));
    if (i < 0 || mapRects[i].id === -1) return;
    const r = mapRects[i];
    itemMenu(e.clientX, e.clientY, mapPath(i), r.d, r.d ? r.id : 0);
  });

  let timer = 0;
  new ResizeObserver(() => { clearTimeout(timer); timer = setTimeout(drawMap, 60); }).observe(wrap);

  // Draggable divider between tree and treemap.
  const splitter = $('#splitter');
  const tree = $('#tree-wrap');
  try { const saved = localStorage.getItem('canopy.split'); if (saved) tree.style.flexBasis = saved; } catch { /* ignore */ }
  splitter.addEventListener('mousedown', (e) => {
    e.preventDefault();
    splitter.classList.add('drag');
    const view = $('#view-folders').getBoundingClientRect();
    const move = (ev) => {
      const pct = Math.max(15, Math.min(85, ((ev.clientY - view.top) / view.height) * 100));
      tree.style.flexBasis = pct + '%';
    };
    const up = () => {
      splitter.classList.remove('drag');
      removeEventListener('mousemove', move);
      removeEventListener('mouseup', up);
      try { localStorage.setItem('canopy.split', tree.style.flexBasis); } catch { /* ignore */ }
    };
    addEventListener('mousemove', move);
    addEventListener('mouseup', up);
  });
}
