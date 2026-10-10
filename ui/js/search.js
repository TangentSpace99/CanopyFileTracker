// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

// Search over the names in the index.

import { S, activeRoot } from './state.js';
import { esc } from './format.js';
import { call } from './api.js';
import { $, $$ } from './ui.js';
import { setTab, hitRows, wireHits, HIT_HEAD } from './tabs.js';

// A search starts 220 ms after the last key press. `searchToken` discards the
// results of a query that has been replaced by a newer one.
let searchTimer = 0;
let searchToken = 0;

/** Handles typing in the search box. Fewer than two characters end the search. */
export function onSearchInput() {
  clearTimeout(searchTimer);
  const text = $('#search').value.trim();
  if (text.length < 2) {
    if (S.tab === 'search') setTab('folders');
    return;
  }
  searchTimer = setTimeout(async () => {
    const token = ++searchToken;
    const view = $('#view-search');
    if (S.tab !== 'search') { for (const b of $$('.tab')) b.classList.remove('on'); S.tab = 'search'; for (const n of ['folders', 'files', 'types', 'changes', 'search']) $('#view-' + n).hidden = n !== 'search'; }
    view.innerHTML = '<div class="empty">Searching…</div>';
    const t0 = performance.now();
    const hits = await call('search', { key: S.key, text, limit: 300 });
    if (token !== searchToken || !hits || S.tab !== 'search') return;
    const took = Math.round(performance.now() - t0);
    view.innerHTML = hits.length
      ? `<div class="list-title"><h3>“${esc(text)}”</h3><span>${hits.length === 300 ? 'largest 300 matches' : hits.length + ' matches'} · ${took} ms · click to locate</span></div>${HIT_HEAD}${hitRows(hits)}</tbody></table>`
      : `<div class="empty"><b>No matches for “${esc(text)}”</b>Search looks at file and folder names in ${esc(activeRoot().path)}.</div>`;
    wireHits(view, hits);
  }, 220);
}
