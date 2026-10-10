// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

// Entry point: wires the global event handlers and starts the app.

import { ROOT_ID, LIVE_INTERVAL, S, activeRoot } from './state.js';
import { listen, call } from './api.js';
import { $, $$, showMenu, hideMenu, setMode } from './ui.js';
import { renderSidebar, refreshRoots, loadDrives, addRoot, removeRoot } from './sidebar.js';
import { startScan, onProgress, onDone, liveTick } from './scan.js';
import { selectRoot, navigate, renderStatus } from './nav.js';
import { loadTree, toggle, select, wireTree } from './tree.js';
import { drawMap, wireMap } from './treemap.js';
import { setTab, refreshTab } from './tabs.js';
import { onSearchInput } from './search.js';

/** Attaches every event handler that is not specific to the tree or the treemap. */
function wire() {
  wireTree();
  wireMap();

  $('#roots').addEventListener('click', (e) => {
    const el = e.target.closest('.root');
    if (!el) return;
    if (e.target.closest('.root-x')) removeRoot(el.dataset.key);
    else if (el.dataset.key !== S.key) selectRoot(el.dataset.key);
  });
  $('#drives').addEventListener('click', (e) => {
    const el = e.target.closest('.drive');
    if (el) addRoot(el.dataset.path);
  });
  $('#welcome-drives').addEventListener('click', (e) => {
    const el = e.target.closest('.dcard');
    if (el) addRoot(el.dataset.path);
  });
  const pick = async () => { const p = await call('pick_folder'); if (p) addRoot(p); };
  $('#add-folder').onclick = pick;
  $('#welcome-folder').onclick = pick;
  $('#side-foot').addEventListener('click', (e) => { if (e.target.id === 'elevate') call('relaunch_elevated'); });

  $('#crumbs').addEventListener('click', (e) => {
    const el = e.target.closest('.crumb');
    if (el) { if (S.tab === 'search') setTab('folders'); navigate(Number(el.dataset.id)); }
  });
  const goUp = () => { if (S.node && S.cwd !== ROOT_ID) navigate(S.node.row.parent); };
  $('#up').onclick = goUp;

  $('#metric').addEventListener('click', (e) => {
    const m = e.target.dataset.m;
    if (!m || m === S.metric) return;
    S.metric = m;
    for (const b of $$('#metric button')) b.classList.toggle('on', b.dataset.m === m);
    renderSidebar();
    if (S.node) { renderStatus(); loadTree(true, S.nav).then(drawMap); refreshTab(); }
  });

  $('#refresh').onclick = () => { if (S.key) startScan(S.key, false); };
  $('#refresh-more').onclick = (e) => {
    const b = e.currentTarget.getBoundingClientRect();
    const r = activeRoot();
    if (!r) return;
    showMenu(b.right - 220, b.bottom + 6, [
      { label: 'Refresh now', hint: 'F5', run: () => startScan(S.key, false) },
      { label: 'Full rescan', hint: 'Shift F5', run: () => startScan(S.key, true) },
      r.scanning ? { label: 'Stop scan', run: () => call('cancel_scan', { key: S.key }) } : null,
      '-',
      { label: 'Remove this index…', run: () => removeRoot(S.key) },
    ]);
    e.stopPropagation();
  };
  $('#scan-cancel').onclick = () => call('cancel_scan', { key: S.key });

  $('#tabs').addEventListener('click', (e) => {
    const b = e.target.closest('.tab');
    if (!b) return;
    $('#search').value = '';
    setTab(b.dataset.tab);
  });
  $('#baseline').onchange = (e) => {
    S.baseline = Number(e.target.value);
    S.baselineAuto = false;
    if (S.node) navigate(S.cwd, true);
  };

  $('#search').addEventListener('input', onSearchInput);

  addEventListener('mousedown', (e) => { if (!e.target.closest('#menu')) hideMenu(); });
  addEventListener('blur', hideMenu);
  addEventListener('contextmenu', (e) => e.preventDefault());
  addEventListener('focus', liveTick);
  addEventListener('mouseup', (e) => { if (e.button === 3) { e.preventDefault(); goUp(); } });

  addEventListener('keydown', (e) => {
    const typing = document.activeElement === $('#search');
    if (e.key === 'Escape') {
      hideMenu();
      if (typing || S.tab === 'search') { $('#search').value = ''; onSearchInput(); $('#tree-scroll').focus(); }
    } else if ((e.ctrlKey && e.key.toLowerCase() === 'f') || (e.key === '/' && !typing)) {
      e.preventDefault(); $('#search').focus(); $('#search').select();
    } else if (e.key === 'F5') {
      e.preventDefault(); if (S.key) startScan(S.key, e.shiftKey);
    } else if (!typing && (e.key === 'Backspace' || (e.altKey && (e.key === 'ArrowLeft' || e.key === 'ArrowUp')))) {
      e.preventDefault(); goUp();
    }
  });
}

/** Start-up: wire the UI, subscribe to scan events, load roots and drives, open the last used root. */
async function boot() {
  wire();
  // A handle for the DevTools console and for automated UI checks. Modules keep
  // their names private, so without this nothing could be reached from outside.
  window.canopy = { S, navigate, setTab, selectRoot, startScan, toggle, select };
  setMode('welcome');
  await listen('scan-progress', (e) => onProgress(e.payload));
  await listen('scan-done', (e) => onDone(e.payload));
  S.elevated = !!(await call('is_elevated'));
  await refreshRoots();
  loadDrives();
  let last = null;
  try { last = localStorage.getItem('canopy.root'); } catch { /* ignore */ }
  const first = S.roots.find((r) => r.key === last) || S.roots[0];
  if (first) await selectRoot(first.key);
  setInterval(liveTick, LIVE_INTERVAL);
  setInterval(() => { if (!document.hidden) renderStatus(); }, 30000);
}

boot();
