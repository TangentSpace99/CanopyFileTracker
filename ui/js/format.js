// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

// Pure formatting helpers: sizes, counts, dates and HTML escaping. No state, no DOM.

/** Escapes text for use inside HTML. Every file name passes through this before it reaches innerHTML. */
export const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

/** Byte count in binary units with about three significant digits: 1536 becomes "1.50 KB". */
export function fmtSize(n) {
  const neg = n < 0;
  n = Math.abs(n);
  const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++; }
  const s = i === 0 ? n.toFixed(0) : n >= 100 ? n.toFixed(0) : n >= 10 ? n.toFixed(1) : n.toFixed(2);
  return (neg ? '−' : '') + s + ' ' + units[i];
}

/** Integer with thousands separators. */
export const fmtCount = (n) => Number(n).toLocaleString('en-US');

/** Short count for tight spaces: 2.7M, 606k. */
export function fmtCompact(n) {
  if (n >= 1e6) return (n / 1e6).toFixed(n >= 1e7 ? 0 : 1) + 'M';
  if (n >= 1e4) return Math.round(n / 1e3) + 'k';
  return fmtCount(n);
}

/** Duration in whichever unit reads best. */
export function fmtMs(ms) {
  if (ms < 1000) return ms + ' ms';
  if (ms < 60000) return (ms / 1000).toFixed(1) + ' s';
  return Math.floor(ms / 60000) + ' min ' + Math.round((ms % 60000) / 1000) + ' s';
}

/** Unix seconds to a local date. */
export function fmtDate(ts) {
  if (!ts || ts <= 0) return '';
  return new Date(ts * 1000).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
}

/** Unix seconds to "today 14:05", "yesterday 09:12" or "3 Oct 09:12". */
export function fmtWhen(ts) {
  const d = new Date(ts * 1000);
  const now = new Date();
  const time = d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
  const days = Math.round((new Date(now.getFullYear(), now.getMonth(), now.getDate()) - new Date(d.getFullYear(), d.getMonth(), d.getDate())) / 86400000);
  if (days === 0) return 'today ' + time;
  if (days === 1) return 'yesterday ' + time;
  return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' }) + ' ' + time;
}

/** Unix seconds to a rough "5 min ago". */
export function fmtAgo(ts) {
  if (!ts) return 'never';
  const s = Math.max(0, Date.now() / 1000 - ts);
  if (s < 45) return 'just now';
  if (s < 3600) return Math.round(s / 60) + ' min ago';
  if (s < 86400) return Math.round(s / 3600) + ' h ago';
  return Math.round(s / 86400) + ' d ago';
}
