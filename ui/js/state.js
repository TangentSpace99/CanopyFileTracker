// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

// Shared state and constants. Almost every other module imports from here.

// Row id of the root folder in every index (see db.rs).
export const ROOT_ID = 1;
export const PAGE = 400;          // children fetched per request

// Height of one tree row in pixels. Must match `--row` in style.css, because
// the virtual list computes positions from it.
export const ROW_H = 30;

// Milliseconds between automatic refreshes of a live root.
export const LIVE_INTERVAL = 20000;

// FILE_ATTRIBUTE_REPARSE_POINT: marks junctions and symbolic links.
export const ATTR_REPARSE = 0x400;

/**
 * The whole UI state in one plain object. Functions read and change it directly
 * and then call the render function for the part of the screen they affected.
 * There is no framework and nothing re-renders by itself.
 */
export const S = {
  // Indexed roots, as returned by the `list_roots` command.
  roots: [],
  // Mounted drives, as returned by `list_drives`.
  drives: [],
  key: null,               // active root
  node: null,              // current folder: { row, path, crumbs, child_count }
  // Row id of the folder currently shown.
  cwd: ROOT_ID,
  metric: 'size',          // 'size' | 'alloc'
  // Sort column and direction of the tree.
  sort: 'size',
  desc: true,
  // Visible view: folders | files | types | changes | search.
  tab: 'folders',
  rows: [],                // flattened, currently visible tree rows
  // Selected tree row (one of the objects in `rows`), or null.
  sel: null,
  // History buckets of the active root, newest first.
  snapshots: [],
  // Snapshot id that sizes are compared against; -1 turns comparison off.
  baseline: -1,
  // True until the user picks a baseline; until then the oldest snapshot is used.
  baselineAuto: true,
  scans: {},               // key -> { silent, t0, first }
  // Is Canopy running as administrator?
  elevated: false,
  // Increases with every navigation. An async result that arrives while this has
  // moved on belongs to a folder the user already left, and is thrown away.
  nav: 0,
  gen: 0,                  // bumped whenever the index content changes
};

/** The root currently shown, or null. */
export const activeRoot = () => S.roots.find((r) => r.key === S.key) || null;

/** Size of a row in the unit the user chose: logical size, or size on disk. */
export const val = (r) => (S.metric === 'alloc' ? r.alloc : r.size);

/** The same at the comparison baseline. Null when the folder has not changed since. */
export const oldVal = (r) => (S.metric === 'alloc' ? r.old_alloc : r.old_size);

/** Joins a folder path and a name with exactly one backslash. */
export const joinPath = (dir, name) => (dir.endsWith('\\') ? dir + name : dir + '\\' + name);
