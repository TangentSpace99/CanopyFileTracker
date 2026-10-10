// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

//! Read side: everything the UI asks the index.

use crate::db::{self, ROOT_ID};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::HashMap;

/// One entry of a folder listing, as the UI shows it.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub id: i64,
    pub parent: i64,
    pub name: String,
    pub is_dir: bool,
    pub size: i64,
    pub alloc: i64,
    pub files: i64,
    pub dirs: i64,
    pub mtime: i64,
    pub attrs: u32,
    /// Folder totals at the comparison baseline, when they differ from now.
    pub old_size: Option<i64>,
    pub old_alloc: Option<i64>,
}

// Column list shared by the row queries. `read_row` depends on this order.
const ROW_COLS: &str = "e.id, e.parent, e.name, e.is_dir, e.size, e.alloc, e.files, e.dirs, e.mtime, e.attrs";

/// Maps a result row (the ROW_COLS columns, then old_size and old_alloc) to a `Row`.
fn read_row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        id: r.get(0)?,
        parent: r.get(1)?,
        name: r.get(2)?,
        is_dir: r.get(3)?,
        size: r.get(4)?,
        alloc: r.get(5)?,
        files: r.get(6)?,
        dirs: r.get(7)?,
        mtime: r.get(8)?,
        attrs: r.get(9)?,
        old_size: r.get(10)?,
        old_alloc: r.get(11)?,
    })
}

/// `baseline` is a snapshot id: deltas are measured against the state when that
/// history bucket began. Pass a negative value for "no comparison".
fn baseline_param(baseline: i64) -> i64 {
    if baseline < 0 {
        i64::MAX
    } else {
        baseline
    }
}

/// Lists the direct children of a folder, one page at a time.
///
/// The inner query attaches to every sub-folder the totals it had at the
/// baseline: the oldest `dir_changes` row at or after that snapshot holds
/// exactly those. A folder without such a row has not changed and gets NULL.
/// Sorting happens in the outer query so that the computed change can be a sort key.
pub fn children(
    c: &Connection,
    parent: i64,
    sort: &str,
    desc: bool,
    offset: i64,
    limit: i64,
    baseline: i64,
) -> rusqlite::Result<Vec<Row>> {
    // The sort key comes from the UI. It is mapped to a fixed SQL fragment here
    // and never pasted into the statement as is.
    let col = match sort {
        "name" => "name COLLATE NOCASE",
        "alloc" => "alloc",
        "files" => "files",
        "mtime" => "mtime",
        "delta" => "(size - COALESCE(old_size, size))",
        _ => "size",
    };
    let dir = if desc { "DESC" } else { "ASC" };
    let sql = format!(
        "SELECT * FROM (
            SELECT {ROW_COLS},
              CASE WHEN e.is_dir = 1 THEN (SELECT c.old_size  FROM dir_changes c WHERE c.id = e.id AND c.snap >= ?2 ORDER BY c.snap LIMIT 1) END AS old_size,
              CASE WHEN e.is_dir = 1 THEN (SELECT c.old_alloc FROM dir_changes c WHERE c.id = e.id AND c.snap >= ?2 ORDER BY c.snap LIMIT 1) END AS old_alloc
            FROM entries e WHERE e.parent = ?1
         ) ORDER BY {col} {dir}, name COLLATE NOCASE LIMIT ?3 OFFSET ?4"
    );
    let mut stmt = c.prepare_cached(&sql)?;
    let rows = stmt.query_map(params![parent, baseline_param(baseline), limit, offset], read_row)?;
    rows.collect()
}

/// Number of direct children of a folder.
pub fn child_count(c: &Connection, parent: i64) -> rusqlite::Result<i64> {
    c.prepare_cached("SELECT COUNT(*) FROM entries WHERE parent = ?1")?
        .query_row([parent], |r| r.get(0))
}

/// One step of the breadcrumb trail.
#[derive(Debug, Clone, Serialize)]
pub struct Crumb {
    pub id: i64,
    pub name: String,
}

/// An entry together with what the UI needs to show it as the current location.
#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub row: Row,
    pub path: String,
    /// Root first, the node itself last.
    pub crumbs: Vec<Crumb>,
    pub child_count: i64,
}

/// Loads one entry with its full path and its breadcrumb trail.
pub fn node(c: &Connection, id: i64, baseline: i64) -> rusqlite::Result<Option<Node>> {
    let sql = format!(
        "SELECT {ROW_COLS},
           (SELECT c.old_size  FROM dir_changes c WHERE c.id = e.id AND c.snap >= ?2 ORDER BY c.snap LIMIT 1),
           (SELECT c.old_alloc FROM dir_changes c WHERE c.id = e.id AND c.snap >= ?2 ORDER BY c.snap LIMIT 1)
         FROM entries e WHERE e.id = ?1"
    );
    let Some(row) = c
        .prepare_cached(&sql)?
        .query_row(params![id, baseline_param(baseline)], read_row)
        .optional()?
    else {
        return Ok(None);
    };
    // Collect the ancestors from the entry up to the root, then reverse the order.
    let mut crumbs = vec![Crumb {
        id: row.id,
        name: row.name.clone(),
    }];
    let mut cur = row.parent;
    let mut stmt = c.prepare_cached("SELECT parent, name FROM entries WHERE id = ?1")?;
    while cur != 0 {
        let Some((parent, name)) = stmt
            .query_row([cur], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .optional()?
        else {
            break;
        };
        crumbs.push(Crumb { id: cur, name });
        cur = parent;
    }
    crumbs.reverse();
    let path = db::path_of(c, id)?.unwrap_or_default();
    let child_count = child_count(c, id)?;
    Ok(Some(Node {
        row,
        path,
        crumbs,
        child_count,
    }))
}

// ---------------------------------------------------------------- treemap

/// One rectangle of the treemap, in canvas pixels. Field names are short
/// because thousands of these are sent to the UI per request.
#[derive(Debug, Clone, Serialize)]
pub struct TmRect {
    pub id: i64,
    /// Index of the enclosing rectangle in the returned list, -1 at the top level.
    pub p: i32,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub name: String,
    pub v: i64,
    pub d: bool,
    pub depth: u8,
}

// Limits that keep a treemap request fast: rectangles returned in total, and
// children read per folder (the remainder is drawn as one "smaller items" block).
const TM_MAX_RECTS: usize = 18000;
const TM_FETCH: i64 = 400;

/// Worst aspect ratio in one row of the layout. `sum` is the total area of the
/// row, `max` and `min` its largest and smallest item, `short` the length of
/// the side the row is laid along. 1.0 would be perfect squares.
fn worst(sum: f64, max: f64, min: f64, short: f64) -> f64 {
    let s2 = short * short;
    let sum2 = sum * sum;
    f64::max(s2 * max / sum2, sum2 / (s2 * min))
}

/// Squarified treemap (Bruls, Huizing, van Wijk). `areas` must be sorted descending
/// and sum to the area of the target rectangle. Calls `emit(index, x, y, w, h)`.
fn squarify(
    areas: &[f64],
    mut x: f64,
    mut y: f64,
    mut w: f64,
    mut h: f64,
    mut emit: impl FnMut(usize, f64, f64, f64, f64),
) {
    let mut i = 0;
    while i < areas.len() {
        let short = w.min(h);
        if short <= 0.0 {
            break;
        }
        let first = areas[i];
        let mut sum = first;
        // Keep adding items to the row for as long as that improves its worst
        // aspect ratio. The first item that would make it worse starts the next row.
        let mut best = worst(sum, first, first, short);
        let mut j = i + 1;
        while j < areas.len() {
            let s = sum + areas[j];
            let cand = worst(s, first, areas[j], short);
            if cand > best {
                break;
            }
            best = cand;
            sum = s;
            j += 1;
        }
        // Lay the finished row along the shorter side of the free area, then
        // carry on in the space that is left.
        let thick = sum / short;
        if w >= h {
            let mut cy = y;
            for (k, a) in areas.iter().enumerate().take(j).skip(i) {
                let len = a / thick;
                emit(k, x, cy, thick, len);
                cy += len;
            }
            x += thick;
            w -= thick;
        } else {
            let mut cx = x;
            for (k, a) in areas.iter().enumerate().take(j).skip(i) {
                let len = a / thick;
                emit(k, cx, y, len, thick);
                cx += len;
            }
            y += thick;
            h -= thick;
        }
        i = j;
    }
}

/// Lays out the contents of folder `id` into a `width` x `height` canvas, recursing
/// into sub-folders for as long as their rectangles stay large enough to read.
pub fn treemap(c: &Connection, id: i64, width: f64, height: f64, by_alloc: bool) -> rusqlite::Result<Vec<TmRect>> {
    let mut out: Vec<TmRect> = Vec::new();
    let total: Option<(i64, i64)> = c
        .prepare_cached("SELECT size, alloc FROM entries WHERE id = ?1")?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((size, alloc)) = total else { return Ok(out) };
    let root_value = if by_alloc { alloc } else { size };

    // A folder waiting to be laid out: its rectangle, and the index in `out` of
    // the rectangle that represents it.
    struct Job {
        id: i64,
        index: i32,
        value: i64,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        depth: u8,
    }
    // Breadth-first: big, shallow folders are laid out before small, deep ones,
    // so the rectangle limit cuts off fine detail and never the overall structure.
    let mut jobs = vec![Job {
        id,
        index: -1,
        value: root_value,
        x: 0.0,
        y: 0.0,
        w: width,
        h: height,
        depth: 0,
    }];
    let mut stmt = c.prepare_cached(
        "SELECT id, name, is_dir, size, alloc FROM entries WHERE parent = ?1 ORDER BY size DESC LIMIT ?2",
    )?;

    let mut head = 0;
    while head < jobs.len() && out.len() < TM_MAX_RECTS {
        let job = &jobs[head];
        head += 1;
        let (jid, jindex, jvalue, jdepth) = (job.id, job.index, job.value, job.depth);
        // Nested folders get a title strip and a small inset so the hierarchy reads.
        let (mut x, mut y, mut w, mut h) = (job.x, job.y, job.w, job.h);
        if jdepth > 0 {
            let pad = if w > 40.0 && h > 40.0 { 2.0 } else { 1.0 };
            let title = if w > 54.0 && h > 34.0 { 15.0 } else { 0.0 };
            x += pad;
            y += pad + title;
            w -= pad * 2.0;
            h -= pad * 2.0 + title;
        }
        if w < 3.0 || h < 3.0 || jvalue <= 0 {
            continue;
        }

        // Largest children first. The index on (parent, size) delivers them in
        // that order without sorting.
        let mut kids: Vec<(i64, String, bool, i64)> = stmt
            .query_map(params![jid, TM_FETCH], |r| {
                let v: i64 = if by_alloc { r.get(4)? } else { r.get(3)? };
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, v))
            })?
            .collect::<rusqlite::Result<_>>()?;
        kids.retain(|k| k.3 > 0);
        if by_alloc {
            kids.sort_by_key(|k| std::cmp::Reverse(k.3));
        }

        // Anything too small to draw is folded into one "everything else" block.
        let area = w * h;
        let scale = area / jvalue as f64;
        let mut shown = 0i64;
        kids.retain(|k| {
            let keep = k.3 as f64 * scale >= 3.0;
            if keep {
                shown += k.3;
            }
            keep
        });
        // `rest` covers the children dropped just above and those beyond the fetch limit.
        let rest = jvalue - shown;
        if rest as f64 * scale >= 3.0 {
            kids.push((-1, String::new(), false, rest));
            kids.sort_by_key(|k| std::cmp::Reverse(k.3));
        }
        if kids.is_empty() {
            continue;
        }
        let sum: i64 = kids.iter().map(|k| k.3).sum();
        let areas: Vec<f64> = kids.iter().map(|k| k.3 as f64 * area / sum as f64).collect();

        let depth = jdepth + 1;
        let mut placed: Vec<(usize, f64, f64, f64, f64)> = Vec::with_capacity(kids.len());
        squarify(&areas, x, y, w, h, |k, rx, ry, rw, rh| placed.push((k, rx, ry, rw, rh)));
        for (k, rx, ry, rw, rh) in placed {
            if rw < 1.0 || rh < 1.0 {
                continue;
            }
            let (kid, name, is_dir, value) = &kids[k];
            let index = out.len() as i32;
            out.push(TmRect {
                id: *kid,
                p: jindex,
                x: rx as f32,
                y: ry as f32,
                w: rw as f32,
                h: rh as f32,
                name: name.clone(),
                v: *value,
                d: *is_dir,
                depth,
            });
            // Only open a folder up if its rectangle can show something inside.
            if *is_dir && rw >= 8.0 && rh >= 8.0 && depth < 12 {
                jobs.push(Job {
                    id: *kid,
                    index,
                    value: *value,
                    x: rx,
                    y: ry,
                    w: rw,
                    h: rh,
                    depth,
                });
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- lists

/// A file or folder found by a list query, with the path of its folder.
#[derive(Debug, Clone, Serialize)]
pub struct FileHit {
    pub id: i64,
    pub parent: i64,
    pub name: String,
    pub dir: String,
    pub is_dir: bool,
    pub size: i64,
    pub alloc: i64,
    pub mtime: i64,
}

/// Turns raw result tuples into `FileHit`s. Many hits share a folder, so each
/// folder path is resolved only once.
fn with_dirs(c: &Connection, rows: Vec<(i64, i64, String, bool, i64, i64, i64)>) -> rusqlite::Result<Vec<FileHit>> {
    let mut cache: HashMap<i64, String> = HashMap::new();
    let mut out = Vec::with_capacity(rows.len());
    for (id, parent, name, is_dir, size, alloc, mtime) in rows {
        let dir = match cache.get(&parent) {
            Some(d) => d.clone(),
            None => {
                let d = db::path_of(c, parent)?.unwrap_or_default();
                cache.insert(parent, d.clone());
                d
            }
        };
        out.push(FileHit {
            id,
            parent,
            name,
            dir,
            is_dir,
            size,
            alloc,
            mtime,
        });
    }
    Ok(out)
}

// (id, parent, name, is_dir, size, alloc, mtime)
type HitTuple = (i64, i64, String, bool, i64, i64, i64);

fn hit_tuple(r: &rusqlite::Row) -> rusqlite::Result<HitTuple> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
    ))
}

/// Recursive CTE selecting every folder at or below `?1`.
const SUBTREE: &str = "WITH RECURSIVE sub(id) AS (
    SELECT ?1
    UNION ALL
    SELECT e.id FROM entries e JOIN sub ON e.parent = sub.id WHERE e.is_dir = 1
)";

/// Largest files at or below folder `id`.
pub fn top_files(c: &Connection, id: i64, limit: i64) -> rusqlite::Result<Vec<FileHit>> {
    let rows: Vec<HitTuple> = if id == ROOT_ID {
        // Whole index: answered straight from the partial "big files" index.
        let mut rows: Vec<HitTuple> = c
            .prepare_cached(
                "SELECT id, parent, name, is_dir, size, alloc, mtime FROM entries
                 WHERE is_dir = 0 AND size >= 1048576 ORDER BY size DESC LIMIT ?1",
            )?
            .query_map([limit], hit_tuple)?
            .collect::<rusqlite::Result<_>>()?;
        if (rows.len() as i64) < limit {
            rows = c
                .prepare_cached(
                    "SELECT id, parent, name, is_dir, size, alloc, mtime FROM entries
                     WHERE is_dir = 0 ORDER BY size DESC LIMIT ?1",
                )?
                .query_map([limit], hit_tuple)?
                .collect::<rusqlite::Result<_>>()?;
        }
        rows
    } else {
        let sql = format!(
            "{SUBTREE}
             SELECT e.id, e.parent, e.name, e.is_dir, e.size, e.alloc, e.mtime
             FROM sub JOIN entries e ON e.parent = sub.id
             WHERE e.is_dir = 0 ORDER BY e.size DESC LIMIT ?2"
        );
        let rows = c
            .prepare_cached(&sql)?
            .query_map(params![id, limit], hit_tuple)?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    with_dirs(c, rows)
}

/// Totals for one file extension.
#[derive(Debug, Clone, Serialize)]
pub struct TypeStat {
    pub ext: String,
    pub count: i64,
    pub size: i64,
    pub alloc: i64,
}

/// Lower-cased extension of a file name, or an empty string when the part
/// after the last dot does not look like an extension.
fn extension(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => {
            let ext = &name[i + 1..];
            if ext.len() <= 10 && ext.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                ext.to_ascii_lowercase()
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

/// File-type breakdown of everything at or below folder `id`, largest first.
pub fn types(c: &Connection, id: i64) -> rusqlite::Result<Vec<TypeStat>> {
    let files_in = |id: i64| -> rusqlite::Result<i64> {
        Ok(c.prepare_cached("SELECT files FROM entries WHERE id = ?1")?
            .query_row([id], |r| r.get(0))
            .optional()?
            .unwrap_or(0))
    };
    let mut map: HashMap<String, (i64, i64, i64)> = HashMap::new();
    let mut tally = |name: &str, size: i64, alloc: i64| {
        let e = map.entry(extension(name)).or_default();
        e.0 += 1;
        e.1 += size;
        e.2 += alloc;
    };

    if id == ROOT_ID || files_in(id)? * 5 > files_in(ROOT_ID)? {
        // A large share of the index: one sequential pass over the table beats
        // millions of index probes. Sub-folders are filtered by a set of folder ids.
        let scope: Option<std::collections::HashSet<i64>> = if id == ROOT_ID {
            None
        } else {
            let sql = format!("{SUBTREE} SELECT id FROM sub");
            let ids = c
                .prepare_cached(&sql)?
                .query_map([id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            Some(ids)
        };
        let mut stmt = c.prepare_cached("SELECT parent, name, size, alloc FROM entries WHERE is_dir = 0")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            if let Some(scope) = &scope {
                if !scope.contains(&r.get::<_, i64>(0)?) {
                    continue;
                }
            }
            tally(r.get_ref(1)?.as_str().unwrap_or(""), r.get(2)?, r.get(3)?);
        }
    } else {
        let sql = format!(
            "{SUBTREE} SELECT e.name, e.size, e.alloc FROM sub JOIN entries e ON e.parent = sub.id WHERE e.is_dir = 0"
        );
        let mut stmt = c.prepare_cached(&sql)?;
        let mut rows = stmt.query([id])?;
        while let Some(r) = rows.next()? {
            tally(r.get_ref(0)?.as_str().unwrap_or(""), r.get(1)?, r.get(2)?);
        }
    }

    let mut out: Vec<TypeStat> = map
        .into_iter()
        .map(|(ext, (count, size, alloc))| TypeStat {
            ext,
            count,
            size,
            alloc,
        })
        .collect();
    out.sort_by_key(|t| std::cmp::Reverse(t.size));
    Ok(out)
}

/// Case-insensitive substring search over every indexed name, largest first.
pub fn search(c: &Connection, text: &str, limit: i64) -> rusqlite::Result<Vec<FileHit>> {
    // `%` and `_` are wildcards in LIKE. Escaping makes them match literally.
    let escaped = text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    let pattern = format!("%{escaped}%");
    let rows: Vec<HitTuple> = c
        .prepare_cached(
            "SELECT id, parent, name, is_dir, size, alloc, mtime FROM entries
             WHERE name LIKE ?1 ESCAPE '\\' AND parent <> 0 ORDER BY size DESC LIMIT ?2",
        )?
        .query_map(params![pattern, limit], hit_tuple)?
        .collect::<rusqlite::Result<_>>()?;
    with_dirs(c, rows)
}

// ---------------------------------------------------------------- history

/// One history bucket.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub id: i64,
    pub ts: i64,
    pub mode: String,
    pub ms: i64,
    pub size: i64,
    pub alloc: i64,
    pub files: i64,
    pub dirs: i64,
    pub changed: i64,
}

/// Newest first.
pub fn snapshots(c: &Connection) -> rusqlite::Result<Vec<Snapshot>> {
    let rows = c
        .prepare_cached("SELECT id, ts, mode, ms, size, alloc, files, dirs, changed FROM snapshots ORDER BY id DESC")?
        .query_map([], |r| {
            Ok(Snapshot {
                id: r.get(0)?,
                ts: r.get(1)?,
                mode: r.get(2)?,
                ms: r.get(3)?,
                size: r.get(4)?,
                alloc: r.get(5)?,
                files: r.get(6)?,
                dirs: r.get(7)?,
                changed: r.get(8)?,
            })
        })?
        .collect();
    rows
}

/// One recorded change.
#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub snap: i64,
    pub path: String,
    pub is_dir: bool,
    /// 1 added, 2 removed, 3 resized
    pub kind: i64,
    pub old_size: i64,
    pub new_size: i64,
}

/// Result of a `changes` query: the largest events, plus totals over all of them.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Changes {
    pub events: Vec<Event>,
    pub total: i64,
    pub grown: i64,
    pub shrunk: i64,
}

/// What happened since history bucket `baseline` began, biggest movers first, optionally
/// restricted to paths under `prefix`.
pub fn changes(c: &Connection, baseline: i64, prefix: &str, limit: i64) -> rusqlite::Result<Changes> {
    let escaped = prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    // Events store full paths, so "below this folder" is a prefix match. The
    // separator is part of the prefix, otherwise `C:\A` would also match `C:\AB`.
    let pattern = if escaped.is_empty() || escaped.ends_with("\\\\") {
        format!("{escaped}%")
    } else {
        format!("{escaped}\\\\%")
    };
    let mut out = Changes::default();
    let (total, grown, shrunk): (i64, Option<i64>, Option<i64>) = c
        .prepare_cached(
            "SELECT COUNT(*),
                    SUM(CASE WHEN new_size > old_size THEN new_size - old_size END),
                    SUM(CASE WHEN new_size < old_size THEN old_size - new_size END)
             FROM events WHERE snap >= ?1 AND path LIKE ?2 ESCAPE '\\'",
        )?
        .query_row(params![baseline, pattern], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    out.total = total;
    out.grown = grown.unwrap_or(0);
    out.shrunk = shrunk.unwrap_or(0);
    out.events = c
        .prepare_cached(
            "SELECT snap, path, is_dir, kind, old_size, new_size FROM events
             WHERE snap >= ?1 AND path LIKE ?2 ESCAPE '\\'
             ORDER BY ABS(new_size - old_size) DESC LIMIT ?3",
        )?
        .query_map(params![baseline, pattern, limit], |r| {
            Ok(Event {
                snap: r.get(0)?,
                path: r.get(1)?,
                is_dir: r.get(2)?,
                kind: r.get(3)?,
                old_size: r.get(4)?,
                new_size: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(out)
}

// ---------------------------------------------------------------- root overview

/// Headline numbers of an index, for the sidebar and the status bar.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Overview {
    pub complete: bool,
    pub size: i64,
    pub alloc: i64,
    pub files: i64,
    pub dirs: i64,
    pub last_scan: i64,
    pub last_mode: String,
    pub last_ms: i64,
    pub last_errors: i64,
    pub has_checkpoint: bool,
}

/// Reads the headline numbers. An index whose first scan never finished
/// reports `complete: false` and nothing else.
pub fn overview(c: &Connection) -> rusqlite::Result<Overview> {
    let mut o = Overview {
        complete: db::meta_get(c, "complete").as_deref() == Some("1"),
        ..Default::default()
    };
    if !o.complete {
        return Ok(o);
    }
    if let Some((size, alloc, files, dirs)) = c
        .prepare_cached("SELECT size, alloc, files, dirs FROM entries WHERE id = ?1")?
        .query_row([ROOT_ID], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?
    {
        o.size = size;
        o.alloc = alloc;
        o.files = files;
        o.dirs = dirs;
    }
    o.last_scan = db::meta_i64(c, "last_scan").unwrap_or(0);
    o.last_mode = db::meta_get(c, "last_mode").unwrap_or_default();
    o.last_ms = db::meta_i64(c, "last_ms").unwrap_or(0);
    o.last_errors = db::meta_i64(c, "last_errors").unwrap_or(0);
    o.has_checkpoint = db::meta_get(c, "journal_id").is_some();
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_lowercased_and_sanity_checked() {
        assert_eq!(extension("photo.JPG"), "jpg");
        assert_eq!(extension("archive.tar.gz"), "gz");
        assert_eq!(extension("Makefile"), "");
        assert_eq!(
            extension(".gitignore"),
            "",
            "a leading dot is a hidden file, not an extension"
        );
        assert_eq!(extension("trailing."), "");
        assert_eq!(extension("notes.final draft"), "", "spaces mean it is not an extension");
        assert_eq!(extension("x.averyveryverylongsuffix"), "");
    }

    fn layout(values: &[f64], w: f64, h: f64) -> Vec<(usize, f64, f64, f64, f64)> {
        let total: f64 = values.iter().sum();
        let areas: Vec<f64> = values.iter().map(|v| v / total * w * h).collect();
        let mut out = Vec::new();
        squarify(&areas, 0.0, 0.0, w, h, |i, x, y, rw, rh| out.push((i, x, y, rw, rh)));
        out
    }

    #[test]
    fn squarify_places_every_item_with_its_exact_area() {
        let values = [50.0, 20.0, 12.0, 8.0, 5.0, 3.0, 2.0];
        let (w, h) = (640.0, 360.0);
        let rects = layout(&values, w, h);
        assert_eq!(rects.len(), values.len());
        let total: f64 = values.iter().sum();
        for &(i, x, y, rw, rh) in &rects {
            let want = values[i] / total * w * h;
            assert!((rw * rh - want).abs() < 1e-6 * want, "item {i} has the wrong area");
            assert!(
                x >= -1e-9 && y >= -1e-9 && x + rw <= w + 1e-6 && y + rh <= h + 1e-6,
                "item {i} leaves the canvas"
            );
        }
    }

    #[test]
    fn squarify_rectangles_do_not_overlap() {
        let rects = layout(&[30.0, 30.0, 20.0, 10.0, 5.0, 5.0], 500.0, 300.0);
        for (n, a) in rects.iter().enumerate() {
            for b in &rects[n + 1..] {
                let overlap_w = (a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1);
                let overlap_h = (a.2 + a.4).min(b.2 + b.4) - a.2.max(b.2);
                assert!(
                    overlap_w < 1e-6 || overlap_h < 1e-6,
                    "items {} and {} overlap",
                    a.0,
                    b.0
                );
            }
        }
    }

    #[test]
    fn squarify_keeps_equal_items_close_to_square() {
        // Four equal items in a square canvas should come out as four squares.
        for &(_, _, _, rw, rh) in &layout(&[1.0, 1.0, 1.0, 1.0], 200.0, 200.0) {
            assert!((rw / rh - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn squarify_handles_the_empty_and_degenerate_cases() {
        assert!(layout(&[], 100.0, 100.0).is_empty());
        let mut calls = 0;
        squarify(&[10.0], 0.0, 0.0, 0.0, 50.0, |_, _, _, _, _| calls += 1);
        assert_eq!(calls, 0, "a canvas without width holds nothing");
    }
}
