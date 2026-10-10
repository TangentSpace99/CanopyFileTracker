// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

// Small building blocks shared by all views: DOM lookup, toasts, the context
// menu, icons and the switch between the three main screens.

import { S } from './state.js';
import { esc } from './format.js';
import { call } from './api.js';
import { navigate } from './nav.js';
import { setTab } from './tabs.js';

// Short query helpers: `$` returns the first match, `$$` all matches as an array.
export const $ = (sel, el = document) => el.querySelector(sel);
export const $$ = (sel, el = document) => [...el.querySelectorAll(sel)];

/**
 * Shows a message in the corner that disappears by itself.
 * `html` is inserted as markup: escape any file names in it.
 */
export function toast(html, isError = false, ms = 4200) {
  const el = document.createElement('div');
  el.className = 'toast' + (isError ? ' err' : '');
  el.innerHTML = html;
  $('#toasts').appendChild(el);
  setTimeout(() => { el.classList.add('out'); setTimeout(() => el.remove(), 300); }, ms);
}

/**
 * Opens the context menu at (x, y), kept inside the window. `items` holds
 * { label, hint?, run } objects, '-' for a separator, and null entries, which
 * are skipped so that callers can write conditions inline.
 */
export function showMenu(x, y, items) {
  const menu = $('#menu');
  menu.innerHTML = '';
  for (const it of items) {
    if (!it) continue;
    if (it === '-') { menu.appendChild(document.createElement('hr')); continue; }
    const b = document.createElement('button');
    b.innerHTML = esc(it.label) + (it.hint ? `<small>${esc(it.hint)}</small>` : '');
    b.onclick = () => { hideMenu(); it.run(); };
    menu.appendChild(b);
  }
  menu.hidden = false;
  const r = menu.getBoundingClientRect();
  menu.style.left = Math.max(4, Math.min(x, innerWidth - r.width - 6)) + 'px';
  menu.style.top = Math.max(4, Math.min(y, innerHeight - r.height - 6)) + 'px';
}

export const hideMenu = () => { $('#menu').hidden = true; };

/** Context menu for a file or folder. `dirId` is the folder to zoom into; 0 leaves that entry out. */
export function itemMenu(x, y, path, isDir, dirId) {
  showMenu(x, y, [
    { label: 'Show in Explorer', run: () => call('reveal', { path }) },
    { label: isDir ? 'Open folder' : 'Open file', run: () => call('open_path', { path }) },
    { label: 'Copy path', run: () => navigator.clipboard.writeText(path).then(() => toast('Path copied')) },
    dirId ? '-' : null,
    dirId ? { label: 'Zoom into this folder', hint: 'Enter', run: () => { setTab('folders'); navigate(dirId); } } : null,
  ]);
}

/**
 * Switches the main area between 'welcome' (nothing indexed yet), 'scanning'
 * (a first index is being built) and 'browse'. Toolbar parts that make no sense
 * in a mode are hidden.
 */
export function setMode(mode) {
  $('#browse').hidden = mode !== 'browse';
  $('#welcome').hidden = mode !== 'welcome';
  $('#scanning').hidden = mode !== 'scanning';
  const browsing = mode === 'browse';
  for (const id of ['#up', '#crumbs', '#metric']) $(id).style.visibility = browsing ? '' : 'hidden';
  $('.search').style.visibility = browsing ? '' : 'hidden';
  $('.split').style.visibility = S.key && mode !== 'welcome' ? '' : 'hidden';
  if (!browsing) { for (const id of ['#crumbs', '#status-left', '#status-right']) $(id).innerHTML = ''; }
}

// Inline SVG icons.
export const DRIVE_ICON = '<svg viewBox="0 0 16 16" width="15" height="15"><rect x="1.5" y="4" width="13" height="8" rx="2" fill="none" stroke="currentColor" stroke-width="1.4"/><circle cx="11.5" cy="8" r="1" fill="currentColor"/></svg>';
export const FOLDER_ICON = '<svg viewBox="0 0 16 16" width="15" height="15"><path d="M1.5 4.2c0-.7.5-1.2 1.2-1.2h3l1.5 1.6h6.1c.7 0 1.2.5 1.2 1.2v6c0 .7-.5 1.2-1.2 1.2H2.7c-.7 0-1.2-.5-1.2-1.2z" fill="currentColor" opacity=".85"/></svg>';
export const CHEVRON = '<svg viewBox="0 0 16 16" width="12" height="12"><path d="m6 4 4 4-4 4" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>';
