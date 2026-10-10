// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

//! Scanning and diffing.
//!
//! Worker threads only list directories; a single coordinator owns the database
//! connection, reconciles each listing against what the index already knows and
//! writes nothing but the differences. Two drivers share that reconcile step:
//!
//! * `full_scan` walks the whole tree (first scan, or any volume without a journal).
//! * `journal_refresh` asks the NTFS change journal which folders were touched since
//!   the last checkpoint and re-lists only those, then pushes the size deltas up.

use crate::db::{self, ROOT_ID};
use crate::win::{self, DirLister, Journal, RawEntry};
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Why a scan ended without a result.
#[derive(Debug)]
pub enum ScanError {
    /// The index database reported an error.
    Sql(rusqlite::Error),
    /// The caller set the cancel flag.
    Cancelled,
    /// The root folder cannot be opened, or the worker threads died.
    Root(String),
}

impl From<rusqlite::Error> for ScanError {
    fn from(e: rusqlite::Error) -> Self {
        ScanError::Sql(e)
    }
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::Sql(e) => write!(f, "index database error: {e}"),
            ScanError::Cancelled => write!(f, "cancelled"),
            ScanError::Root(m) => write!(f, "{m}"),
        }
    }
}

/// A progress report, sent to the caller about every 80 ms while a scan runs.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    /// "scan" (full walk), "diff" (journal refresh), "aggregate" (summing folder
    /// totals) or "index" (building the database indexes after a first scan).
    pub phase: &'static str,
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub errors: u64,
    /// The folder processed most recently.
    pub current: String,
}

/// Cancellation flag plus a progress sink, handed in by the caller.
pub struct Ctl<'a> {
    pub cancel: &'a AtomicBool,
    pub progress: &'a mut dyn FnMut(&Progress),
}

/// The outcome of a finished scan or refresh.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    /// "full" or "journal".
    pub mode: String,
    /// This run built the index from nothing.
    pub first: bool,
    /// Wall-clock duration in milliseconds.
    pub ms: u64,
    /// Folders actually listed from disk.
    pub listed: u64,
    /// Entries added, removed or resized.
    pub changed: u64,
    /// Folders that could not be read (usually access denied).
    pub errors: u64,
    /// Journal records examined (journal mode only).
    pub records: u64,
    /// History bucket this run's changes were filed under, if it recorded any.
    pub snapshot: Option<i64>,
    /// Totals of the whole root after this run.
    pub size: i64,
    pub alloc: i64,
    pub files: i64,
    pub dirs: i64,
}

/// One folder to list: the unit of work handed to a worker thread.
struct Task {
    id: i64,
    path: String,
    /// The folder is new to the index, so there is nothing to diff against.
    fresh: bool,
}

/// A worker's answer: the folder's entries, or the Win32 error code.
struct Listing {
    task: Task,
    res: Result<Vec<RawEntry>, u32>,
}

/// Starts the worker threads and returns the two ends the coordinator talks to.
///
/// Tasks go out through an unbounded queue. Only the coordinator adds to it, so
/// it can never block itself. Listings come back through a bounded queue: when
/// the coordinator is the slower side, workers wait instead of piling up
/// listings in memory. The threads end on their own once the channels are dropped.
fn spawn_pool() -> (Sender<Task>, Receiver<Listing>) {
    let (task_tx, task_rx) = unbounded::<Task>();
    let (res_tx, res_rx) = bounded::<Listing>(2048);
    // Listing is I/O bound, so more threads than cores still helps on a cold disk,
    // but beyond 16 there was nothing left to gain.
    let n = std::thread::available_parallelism().map_or(8, |n| n.get()).clamp(4, 16);
    for _ in 0..n {
        let rx = task_rx.clone();
        let tx = res_tx.clone();
        std::thread::spawn(move || {
            let mut lister = DirLister::new();
            for task in rx {
                let res = lister.list(&task.path);
                if tx.send(Listing { task, res }).is_err() {
                    break;
                }
            }
        });
    }
    (task_tx, res_rx)
}

/// [size, alloc, files, dirs]
type Totals = [i64; 4];

fn add(a: &mut Totals, b: &Totals) {
    for i in 0..4 {
        a[i] += b[i];
    }
}

fn sub(a: &mut Totals, b: &Totals) {
    for i in 0..4 {
        a[i] -= b[i];
    }
}

/// Per-folder accumulator. `acc` is the folder's absolute totals during a full scan
/// and its change relative to `old` during a journal refresh; for a new folder the
/// two readings coincide, which lets both modes share one bottom-up pass.
#[derive(Clone, Copy)]
struct Agg {
    /// Row id of the containing folder, 0 for the root.
    parent: i64,
    /// Totals the index held before this run (all zero for a new folder).
    old: Totals,
    /// Collected during this run. See the struct comment for its two meanings.
    acc: Totals,
    /// The folder was inserted during this run.
    is_new: bool,
    /// The folder was deleted during this run: nothing is written for it and
    /// nothing is passed up from it.
    dead: bool,
}

/// A child row as the index knew it before this run.
struct Old {
    id: i64,
    is_dir: bool,
    tot: Totals,
    mtime: i64,
    attrs: u32,
    frn: u64,
}

// Event kinds as stored in the `events` table.
const EV_ADDED: i64 = 1;
const EV_REMOVED: i64 = 2;
const EV_RESIZED: i64 = 3;

/// The coordinator: all state of one scan or refresh. It lives on the thread
/// that owns the write connection; worker threads never touch the database.
struct Engine<'a> {
    conn: &'a Connection,
    /// True for a journal refresh, false for a full walk.
    incremental: bool,
    /// The index is being built from nothing, so there is no diffing and no history.
    first: bool,
    /// History bucket this run files its changes under.
    snap: i64,
    /// `snap` continues the newest history bucket instead of opening a new one.
    merge: bool,
    /// Where our own index files live. Writing the index changes that folder, which
    /// must not count as a change or every refresh would trigger the next one.
    own_dir: String,
    /// Set while the folder being reconciled is `own_dir`.
    muted: bool,
    /// Next free row id. Ids only ever grow, so a child always has a larger id
    /// than its parent; `finish` relies on that.
    next_id: i64,
    /// One accumulator for every folder whose totals this run may change.
    aggs: HashMap<i64, Agg>,
    /// Folders that appeared inside already known folders. Each becomes a single
    /// "added" event at the end, when its total size is known.
    new_dirs: Vec<(i64, String)>,
    /// Journal refresh only: folders that could not be opened under their indexed path.
    failed: Vec<(i64, String)>,
    /// Channel ends towards the worker pool.
    task_tx: Sender<Task>,
    res_rx: Receiver<Listing>,
    /// Tasks handed out whose listing has not been processed yet. Zero means done.
    pending: usize,
    /// Rows inserted since the last intermediate commit (first scan only).
    since_commit: u64,
    /// Counters for the summary and the progress reports.
    changed: u64,
    errors: u64,
    listed: u64,
    files_seen: u64,
    bytes_seen: u64,
}

impl<'a> Engine<'a> {
    fn new(conn: &'a Connection, incremental: bool, first: bool) -> rusqlite::Result<Self> {
        let next_id: i64 = conn.query_row("SELECT COALESCE(MAX(id), 1) + 1 FROM entries", [], |r| r.get(0))?;
        // History is bucketed: refreshes landing within one period share a snapshot,
        // so a refresh every few seconds does not flood the timeline.
        let latest: Option<(i64, i64)> = conn
            .query_row("SELECT id, ts FROM snapshots ORDER BY id DESC LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        let (snap, merge) = match latest {
            Some((id, ts)) if !first && now() - ts < db::SNAPSHOT_PERIOD_SECS => (id, true),
            Some((id, _)) => (id + 1, false),
            None => (1, false),
        };
        let (task_tx, res_rx) = spawn_pool();
        Ok(Self {
            conn,
            incremental,
            first,
            snap,
            merge,
            own_dir: db::index_dir().to_string_lossy().into_owned(),
            muted: false,
            next_id,
            aggs: HashMap::new(),
            new_dirs: Vec::new(),
            failed: Vec::new(),
            task_tx,
            res_rx,
            pending: 0,
            since_commit: 0,
            changed: 0,
            errors: 0,
            listed: 0,
            files_seen: 0,
            bytes_seen: 0,
        })
    }

    /// Hands a folder to the workers.
    fn enqueue(&mut self, task: Task) {
        self.pending += 1;
        let _ = self.task_tx.send(task);
    }

    /// Writes one line of history. Nothing is recorded on a first scan (every
    /// entry would be "added") or for our own index folder.
    fn event(&self, path: &str, is_dir: bool, kind: i64, old: i64, new: i64) -> rusqlite::Result<()> {
        if self.first || self.muted {
            return Ok(());
        }
        self.conn
            .prepare_cached(
                "INSERT INTO events(snap, path, is_dir, kind, old_size, new_size) VALUES(?1,?2,?3,?4,?5,?6)",
            )?
            .execute(params![self.snap, path, is_dir, kind, old, new])?;
        Ok(())
    }

    /// Parent id and stored totals of one folder, or `None` if the row is gone.
    fn load_totals(&self, id: i64) -> rusqlite::Result<Option<(i64, Totals)>> {
        self.conn
            .prepare_cached("SELECT parent, size, alloc, files, dirs FROM entries WHERE id = ?1")?
            .query_row([id], |r| Ok((r.get(0)?, [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?])))
            .optional()
    }

    /// Registers an existing folder and every ancestor, so deltas have a path to the root.
    fn ensure_agg(&mut self, mut id: i64) -> rusqlite::Result<bool> {
        let mut found = true;
        while id != 0 && !self.aggs.contains_key(&id) {
            let Some((parent, old)) = self.load_totals(id)? else {
                found = false;
                break;
            };
            self.aggs.insert(
                id,
                Agg {
                    parent,
                    old,
                    acc: [0; 4],
                    is_new: false,
                    dead: false,
                },
            );
            id = parent;
        }
        Ok(found)
    }

    /// Everything the index currently holds directly inside `parent`, keyed by name.
    fn load_children(&self, parent: i64) -> rusqlite::Result<HashMap<String, Old>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, name, is_dir, size, alloc, files, dirs, mtime, attrs, frn FROM entries WHERE parent = ?1",
        )?;
        let rows = stmt.query_map([parent], |r| {
            Ok((
                r.get::<_, String>(1)?,
                Old {
                    id: r.get(0)?,
                    is_dir: r.get(2)?,
                    tot: [r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?],
                    mtime: r.get(7)?,
                    attrs: r.get(8)?,
                    frn: r.get::<_, i64>(9)? as u64,
                },
            ))
        })?;
        rows.collect()
    }

    /// Inserts an entry the index has never seen. Returns its direct contribution
    /// to the parent (a new folder's contents arrive later through its own `Agg`).
    fn insert_new(&mut self, task: &Task, e: &RawEntry) -> rusqlite::Result<Totals> {
        let id = self.next_id;
        self.next_id += 1;
        self.since_commit += 1;
        let (size, alloc) = if e.is_dir {
            (0, 0)
        } else {
            (e.size as i64, e.alloc as i64)
        };
        self.conn
            .prepare_cached(
                "INSERT INTO entries(id, parent, name, is_dir, size, alloc, files, dirs, mtime, attrs, frn)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, 0, 0, ?7, ?8, ?9)",
            )?
            .execute(params![
                id,
                task.id,
                e.name,
                e.is_dir,
                size,
                alloc,
                e.mtime,
                e.attrs,
                e.frn as i64
            ])?;
        if !e.is_dir {
            self.files_seen += 1;
            self.bytes_seen += e.size;
            if !task.fresh {
                self.event(&win::join(&task.path, &e.name), false, EV_ADDED, 0, size)?;
            }
            return Ok([size, alloc, 1, 0]);
        }
        self.aggs.insert(
            id,
            Agg {
                parent: task.id,
                old: [0; 4],
                acc: [0; 4],
                is_new: true,
                dead: false,
            },
        );
        let path = win::join(&task.path, &e.name);
        if !task.fresh && !self.first {
            self.new_dirs.push((id, path.clone()));
        }
        if !e.is_reparse() {
            self.enqueue(Task { id, path, fresh: true });
        }
        Ok([0, 0, 0, 1])
    }

    /// An entry that exists both on disk and in the index. Returns (absolute, delta).
    fn matched(&mut self, task: &Task, o: &Old, e: &RawEntry) -> rusqlite::Result<(Totals, Totals)> {
        let meta_changed = o.mtime != e.mtime || o.attrs != e.attrs || o.frn != e.frn;
        if e.is_dir {
            if meta_changed {
                self.conn
                    .prepare_cached("UPDATE entries SET mtime = ?2, attrs = ?3, frn = ?4 WHERE id = ?1")?
                    .execute(params![o.id, e.mtime, e.attrs, e.frn as i64])?;
            }
            // A full walk descends into every known folder. A journal refresh does
            // not: had anything changed in there, the journal would have named that
            // folder itself.
            if !self.incremental {
                self.aggs.insert(
                    o.id,
                    Agg {
                        parent: task.id,
                        old: o.tot,
                        acc: [0; 4],
                        is_new: false,
                        dead: false,
                    },
                );
                if !e.is_reparse() {
                    self.enqueue(Task {
                        id: o.id,
                        path: win::join(&task.path, &e.name),
                        fresh: false,
                    });
                }
            }
            return Ok(([0, 0, 0, 1], [0; 4]));
        }
        let (size, alloc) = (e.size as i64, e.alloc as i64);
        self.files_seen += 1;
        self.bytes_seen += e.size;
        // Only a size change counts as a change. A new timestamp or attribute is
        // written to the index without being reported.
        let resized = size != o.tot[0] || alloc != o.tot[1];
        if resized || meta_changed {
            self.conn
                .prepare_cached(
                    "UPDATE entries SET size = ?2, alloc = ?3, mtime = ?4, attrs = ?5, frn = ?6 WHERE id = ?1",
                )?
                .execute(params![o.id, size, alloc, e.mtime, e.attrs, e.frn as i64])?;
        }
        if resized {
            self.changed += 1;
            self.event(&win::join(&task.path, &e.name), false, EV_RESIZED, o.tot[0], size)?;
        }
        Ok(([size, alloc, 1, 0], [size - o.tot[0], alloc - o.tot[1], 0, 0]))
    }

    /// Removes a folder and everything below it from the index, one level at a time.
    fn delete_subtree(&mut self, id: i64) -> rusqlite::Result<()> {
        let conn = self.conn;
        let mut stack = vec![id];
        while let Some(dir) = stack.pop() {
            if let Some(a) = self.aggs.get_mut(&dir) {
                a.dead = true;
            }
            {
                let mut q = conn.prepare_cached("SELECT id FROM entries WHERE parent = ?1 AND is_dir = 1")?;
                for child in q.query_map([dir], |r| r.get::<_, i64>(0))? {
                    stack.push(child?);
                }
            }
            conn.prepare_cached("DELETE FROM entries WHERE parent = ?1")?
                .execute([dir])?;
        }
        conn.prepare_cached("DELETE FROM entries WHERE id = ?1")?
            .execute([id])?;
        Ok(())
    }

    /// Brings the index in line with one directory listing. This wrapper only takes
    /// care of muting our own index folder; the work happens in `reconcile_inner`.
    fn reconcile(&mut self, task: &Task, entries: Vec<RawEntry>) -> rusqlite::Result<()> {
        self.muted = task.path.eq_ignore_ascii_case(&self.own_dir);
        let changed_before = self.changed;
        let result = self.reconcile_inner(task, entries);
        if self.muted {
            self.changed = changed_before;
            self.muted = false;
        }
        result
    }

    /// Compares a listing with the index rows of the same folder and writes the
    /// differences.
    ///
    /// Two sets of numbers are collected on the way: `abs`, the folder's direct
    /// content as listed, and `delta`, how that content differs from the index.
    /// A full walk uses the first, a journal refresh the second.
    fn reconcile_inner(&mut self, task: &Task, entries: Vec<RawEntry>) -> rusqlite::Result<()> {
        self.listed += 1;
        let mut abs: Totals = [0; 4];
        let mut delta: Totals = [0; 4];

        // The folder itself is new to the index, so everything in it is new too
        // and nothing has to be looked up.
        if task.fresh {
            for e in &entries {
                let c = self.insert_new(task, e)?;
                add(&mut abs, &c);
            }
        } else {
            // Pass 1: match by name. What remains in `old` afterwards no longer
            // exists on disk; `unmatched` collects what the index has not seen yet.
            let mut old = self.load_children(task.id)?;
            let mut unmatched: Vec<RawEntry> = Vec::new();
            for e in entries {
                match old.remove(&e.name) {
                    Some(o) if o.is_dir == e.is_dir => {
                        let (a, d) = self.matched(task, &o, &e)?;
                        add(&mut abs, &a);
                        add(&mut delta, &d);
                    }
                    Some(o) => {
                        // Same name, different kind: the old one is gone, the new one is new.
                        old.insert(e.name.clone(), o);
                        unmatched.push(e);
                    }
                    None => unmatched.push(e),
                }
            }

            // A folder that vanished under one name and appeared under another with the
            // same file id was renamed: keep its rows instead of rescanning its subtree.
            let mut by_frn: HashMap<u64, String> = old
                .iter()
                .filter(|(_, o)| o.is_dir && o.frn != 0)
                .map(|(name, o)| (o.frn, name.clone()))
                .collect();
            // Pass 2: insert the new entries, except folders recognised as renamed.
            for e in &unmatched {
                if e.is_dir && e.frn != 0 {
                    if let Some(old_name) = by_frn.remove(&e.frn) {
                        let o = old.remove(&old_name).expect("renamed folder is in the old set");
                        self.conn
                            .prepare_cached("UPDATE entries SET name = ?2 WHERE id = ?1")?
                            .execute(params![o.id, e.name])?;
                        self.changed += 1;
                        let (a, d) = self.matched(task, &o, e)?;
                        add(&mut abs, &a);
                        add(&mut delta, &d);
                        continue;
                    }
                }
                self.changed += 1;
                let c = self.insert_new(task, e)?;
                add(&mut abs, &c);
                add(&mut delta, &c);
            }

            // Pass 3: whatever is still in `old` has been deleted.
            for (name, o) in old {
                self.changed += 1;
                let path = win::join(&task.path, &name);
                if o.is_dir {
                    self.delete_subtree(o.id)?;
                    sub(&mut delta, &[o.tot[0], o.tot[1], o.tot[2], o.tot[3] + 1]);
                } else {
                    self.conn
                        .prepare_cached("DELETE FROM entries WHERE id = ?1")?
                        .execute([o.id])?;
                    sub(&mut delta, &[o.tot[0], o.tot[1], 1, 0]);
                }
                self.event(&path, o.is_dir, EV_REMOVED, o.tot[0], 0)?;
            }
        }

        // Hand this folder's numbers to its accumulator. `finish` adds them up the tree.
        let contribution = if self.incremental && !task.fresh { delta } else { abs };
        if let Some(a) = self.aggs.get_mut(&task.id) {
            add(&mut a.acc, &contribution);
        }
        Ok(())
    }

    /// A folder could not be listed.
    fn on_error(&mut self, task: Task, code: u32) {
        let gone = code == win::ERROR_FILE_NOT_FOUND || code == win::ERROR_PATH_NOT_FOUND;
        if self.incremental && !task.fresh {
            // Possibly renamed or moved while we were looking; retried once with a fresh path.
            self.failed.push((task.id, task.path));
            return;
        }
        if !gone {
            self.errors += 1;
        }
        if !task.fresh {
            // Could not re-read a folder we already know: keep what we had rather than zeroing it.
            if let Some(a) = self.aggs.get_mut(&task.id) {
                a.acc = a.old;
            }
        }
    }

    /// Processes listings until every queued folder has been handled.
    fn drive(&mut self, ctl: &mut Ctl) -> Result<(), ScanError> {
        let mut last_tick = Instant::now();
        while self.pending > 0 {
            // Blocks until some worker has finished a folder.
            let listing = self
                .res_rx
                .recv()
                .map_err(|_| ScanError::Root("scanner threads stopped".into()))?;
            self.pending -= 1;
            if ctl.cancel.load(Ordering::Relaxed) {
                return Err(ScanError::Cancelled);
            }
            let Listing { task, res } = listing;
            match res {
                Ok(entries) => {
                    // During a journal refresh an earlier reconcile may have removed this folder.
                    if self.incremental && !self.aggs.get(&task.id).is_some_and(|a| !a.dead) {
                        continue;
                    }
                    self.reconcile(&task, entries)?;
                    if last_tick.elapsed() >= Duration::from_millis(80) {
                        last_tick = Instant::now();
                        (ctl.progress)(&Progress {
                            phase: if self.incremental { "diff" } else { "scan" },
                            files: self.files_seen,
                            dirs: self.listed,
                            bytes: self.bytes_seen,
                            errors: self.errors,
                            current: task.path,
                        });
                    }
                }
                Err(code) => self.on_error(task, code),
            }
            // The very first load is committed in chunks so the WAL stays small.
            if self.first && self.since_commit >= 250_000 {
                self.since_commit = 0;
                self.conn.execute_batch("COMMIT; BEGIN IMMEDIATE")?;
            }
        }
        Ok(())
    }

    /// Bottom-up pass: children always have larger ids than their parents, so walking
    /// ids in descending order visits every folder after all of its descendants.
    fn finish(mut self, mode: &str, t0: Instant, records: u64, ctl: &mut Ctl) -> Result<Summary, ScanError> {
        let conn = self.conn;
        (ctl.progress)(&Progress {
            phase: "aggregate",
            files: self.files_seen,
            dirs: self.listed,
            bytes: self.bytes_seen,
            errors: self.errors,
            current: String::new(),
        });

        // Pass every folder's numbers up to its parent, deepest folders first, and
        // write the totals that changed.
        let mut ids: Vec<i64> = self.aggs.keys().copied().collect();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        {
            let mut update =
                conn.prepare_cached("UPDATE entries SET size = ?2, alloc = ?3, files = ?4, dirs = ?5 WHERE id = ?1")?;
            let mut history = conn.prepare_cached(
                "INSERT OR IGNORE INTO dir_changes(id, snap, old_size, old_alloc, old_files) VALUES(?1, ?2, ?3, ?4, ?5)",
            )?;
            for id in ids {
                let a = self.aggs[&id];
                if a.dead {
                    continue;
                }
                if let Some(p) = self.aggs.get_mut(&a.parent) {
                    add(&mut p.acc, &a.acc);
                }
                // Full walk: `acc` already is the new total. Journal refresh: `acc`
                // is a difference that has to be added to the stored total.
                let mut total = a.acc;
                if self.incremental && !a.is_new {
                    add(&mut total, &a.old);
                }
                if total != a.old {
                    update.execute(params![id, total[0], total[1], total[2], total[3]])?;
                    if !self.first {
                        history.execute(params![id, self.snap, a.old[0], a.old[1], a.old[2]])?;
                    }
                }
            }
        }

        // The size of a newly found folder is known only now, so its event comes last.
        for (id, path) in std::mem::take(&mut self.new_dirs) {
            if let Some(a) = self.aggs.get(&id) {
                self.event(&path, true, EV_ADDED, 0, a.acc[0])?;
            }
        }

        if self.first {
            (ctl.progress)(&Progress {
                phase: "index",
                files: self.files_seen,
                dirs: self.listed,
                bytes: self.bytes_seen,
                errors: self.errors,
                current: String::new(),
            });
            db::create_indexes(conn)?;
        }

        let tot: Totals = conn.query_row(
            "SELECT size, alloc, files, dirs FROM entries WHERE id = ?1",
            [ROOT_ID],
            |r| Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?]),
        )?;
        let ms = t0.elapsed().as_millis() as u64;
        // File this run under a history bucket: continue the current one, open a new
        // one, or, when nothing changed, none at all.
        let snapshot = if self.merge {
            conn.execute(
                "UPDATE snapshots SET mode = ?2, ms = ?3, size = ?4, alloc = ?5, files = ?6, dirs = ?7, changed = changed + ?8 WHERE id = ?1",
                params![self.snap, mode, ms as i64, tot[0], tot[1], tot[2], tot[3], self.changed as i64],
            )?;
            Some(self.snap)
        } else if self.first || self.changed > 0 {
            conn.execute(
                "INSERT INTO snapshots(id, ts, mode, ms, size, alloc, files, dirs, changed) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![self.snap, now(), mode, ms as i64, tot[0], tot[1], tot[2], tot[3], self.changed as i64],
            )?;
            // Drop history beyond the retention limit.
            let cutoff = self.snap - db::KEEP_SNAPSHOTS;
            if cutoff > 0 {
                conn.execute("DELETE FROM snapshots WHERE id <= ?1", [cutoff])?;
                conn.execute("DELETE FROM events WHERE snap <= ?1", [cutoff])?;
                conn.execute("DELETE FROM dir_changes WHERE snap <= ?1", [cutoff])?;
            }
            Some(self.snap)
        } else {
            None
        };

        db::meta_set(conn, "complete", "1")?;
        db::meta_set(conn, "last_scan", &now().to_string())?;
        db::meta_set(conn, "last_mode", mode)?;
        db::meta_set(conn, "last_ms", &ms.to_string())?;
        db::meta_set(conn, "last_errors", &self.errors.to_string())?;

        Ok(Summary {
            mode: mode.to_string(),
            first: self.first,
            ms,
            listed: self.listed,
            changed: self.changed,
            errors: self.errors,
            records,
            snapshot,
            size: tot[0],
            alloc: tot[1],
            files: tot[2],
            dirs: tot[3],
        })
    }
}

/// Current time in unix seconds.
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Runs `f` inside one write transaction. Readers (the UI) keep seeing the old
/// state until the commit, then see all of a run's changes at once.
fn in_tx<T>(conn: &Connection, f: impl FnOnce() -> Result<T, ScanError>) -> Result<T, ScanError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    match f() {
        Ok(v) => {
            conn.execute_batch("COMMIT")?;
            Ok(v)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// `C:\...` -> `C`, if the root lives on a lettered NTFS volume.
fn journal_volume(root: &str) -> Option<(char, u32)> {
    let b = root.as_bytes();
    if b.len() < 3 || b[1] != b':' || !b[0].is_ascii_alphabetic() {
        return None;
    }
    let letter = b[0] as char;
    let vol = win::volume_info(&format!("{letter}:\\"))?;
    (vol.fs == "NTFS").then_some((letter, vol.serial))
}

/// Whether a diff refresh of `root` could use the change journal right now.
pub fn journal_available(root: &str) -> bool {
    journal_volume(root).is_some_and(|(letter, _)| Journal::open(letter).is_ok())
}

/// Walks the whole tree. The index ends up identical to the disk, but only entries
/// that actually differ are written.
pub fn full_scan(conn: &Connection, root: &str, ctl: &mut Ctl) -> Result<Summary, ScanError> {
    let t0 = Instant::now();
    let (frn, attrs, mtime) =
        win::stat_dir(root).map_err(|e| ScanError::Root(format!("cannot open {root} (Windows error {e})")))?;
    // No completed index yet (never scanned, or a first scan was interrupted):
    // start from an empty database.
    let first = db::meta_get(conn, "complete").as_deref() != Some("1");
    if first {
        db::reset(conn)?;
    }
    db::meta_set(conn, "root", root)?;

    // Checkpoint the journal *before* walking: anything that changes while we scan
    // is replayed by the next refresh.
    let checkpoint = journal_volume(root)
        .and_then(|(letter, serial)| Journal::open(letter).ok().map(|j| (j.id, j.next_usn, serial)));

    // The root folder is always row 1; everything else hangs below it.
    in_tx(conn, || {
        let mut eng = Engine::new(conn, false, first)?;
        if first {
            conn.execute(
                "INSERT INTO entries(id, parent, name, is_dir, size, alloc, files, dirs, mtime, attrs, frn)
                 VALUES(?1, 0, ?2, 1, 0, 0, 0, 0, ?3, ?4, ?5)",
                params![ROOT_ID, root, mtime, attrs, frn as i64],
            )?;
            eng.aggs.insert(
                ROOT_ID,
                Agg {
                    parent: 0,
                    old: [0; 4],
                    acc: [0; 4],
                    is_new: true,
                    dead: false,
                },
            );
        } else {
            conn.execute(
                "UPDATE entries SET mtime = ?2, attrs = ?3, frn = ?4 WHERE id = ?1",
                params![ROOT_ID, mtime, attrs, frn as i64],
            )?;
            let old = eng.load_totals(ROOT_ID)?.map_or([0; 4], |(_, t)| t);
            eng.aggs.insert(
                ROOT_ID,
                Agg {
                    parent: 0,
                    old,
                    acc: [0; 4],
                    is_new: false,
                    dead: false,
                },
            );
        }
        eng.enqueue(Task {
            id: ROOT_ID,
            path: root.to_string(),
            fresh: first,
        });
        eng.drive(ctl)?;
        let summary = eng.finish("full", t0, 0, ctl)?;

        match checkpoint {
            Some((id, usn, serial)) => {
                db::meta_set(conn, "journal_id", &id.to_string())?;
                db::meta_set(conn, "journal_usn", &usn.to_string())?;
                db::meta_set(conn, "serial", &serial.to_string())?;
            }
            None => {
                conn.execute(
                    "DELETE FROM meta WHERE k IN ('journal_id', 'journal_usn', 'serial')",
                    [],
                )?;
            }
        }
        Ok(summary)
    })
}

/// Diff refresh driven by the NTFS change journal. Returns `Ok(None)` when the
/// journal cannot answer (no journal, no checkpoint, checkpoint expired), in which
/// case the caller falls back to `full_scan`.
pub fn journal_refresh(conn: &Connection, root: &str, ctl: &mut Ctl) -> Result<Option<Summary>, ScanError> {
    let t0 = Instant::now();
    if db::meta_get(conn, "complete").as_deref() != Some("1") {
        return Ok(None);
    }
    // A refresh needs the checkpoint the previous run left behind, and it must
    // still refer to the same journal on the same volume. If anything is off we
    // return `None` and the caller does a full scan.
    let stored_id = db::meta_get(conn, "journal_id").and_then(|v| v.parse::<u64>().ok());
    let (Some(stored_id), Some(stored_usn)) = (stored_id, db::meta_i64(conn, "journal_usn")) else {
        return Ok(None);
    };
    let Some((letter, serial)) = journal_volume(root) else {
        return Ok(None);
    };
    if db::meta_i64(conn, "serial") != Some(serial as i64) {
        return Ok(None);
    }
    let Ok(journal) = Journal::open(letter) else {
        return Ok(None);
    };
    if journal.id != stored_id {
        return Ok(None);
    }

    // Every record names the folder whose listing changed; that is all we need.
    let mut dirty: HashSet<u64> = HashSet::new();
    let mut records = 0u64;
    let Ok(next_usn) = journal.read(stored_usn, |r| {
        records += 1;
        dirty.insert(r.parent_frn);
    }) else {
        return Ok(None);
    };
    drop(journal);

    // Translate the reference numbers from the journal into index rows and queue
    // those folders. Numbers we do not know belong to folders outside this root, or
    // to brand-new folders, which the listing of their parent will reveal.
    in_tx(conn, || {
        let mut eng = Engine::new(conn, true, false)?;
        {
            let mut by_frn = conn.prepare_cached("SELECT id FROM entries WHERE frn = ?1 AND is_dir = 1")?;
            for frn in &dirty {
                let ids: Vec<i64> = by_frn
                    .query_map([*frn as i64], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                for id in ids {
                    if eng.ensure_agg(id)? {
                        if let Some(path) = db::path_of(conn, id)? {
                            eng.enqueue(Task { id, path, fresh: false });
                        }
                    }
                }
            }
        }
        eng.drive(ctl)?;

        // Folders that could not be opened under their indexed path may simply have
        // been renamed by a reconcile above; try again where they live now.
        for (id, tried) in std::mem::take(&mut eng.failed) {
            if eng.aggs.get(&id).is_some_and(|a| !a.dead) {
                if let Some(path) = db::path_of(conn, id)? {
                    if path != tried {
                        eng.enqueue(Task { id, path, fresh: false });
                    }
                }
            }
        }
        eng.drive(ctl)?;

        let summary = eng.finish("journal", t0, records, ctl)?;
        db::meta_set(conn, "journal_usn", &next_usn.to_string())?;
        Ok(Some(summary))
    })
}

/// Brings the index up to date the cheapest way available.
pub fn refresh(conn: &Connection, root: &str, force_full: bool, ctl: &mut Ctl) -> Result<Summary, ScanError> {
    if !force_full {
        if let Some(summary) = journal_refresh(conn, root, ctl)? {
            return Ok(summary);
        }
    }
    full_scan(conn, root, ctl)
}
