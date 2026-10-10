// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

//! End-to-end tests of the scan and diff engine against a real directory tree.
//!
//! Each test builds a small tree in the system temp directory, indexes it into a
//! database that lives *outside* that tree, and checks what the index reports.

use canopy_core::db::{self, ROOT_ID};
use canopy_core::rusqlite::Connection;
use canopy_core::scan::{self, Ctl, Progress, ScanError, Summary};
use canopy_core::{query, win};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// A scratch directory holding the tree to scan (`root`) and its index (`db`).
/// Removed again when dropped.
struct Fixture {
    base: PathBuf,
    root: String,
    conn: Connection,
}

static NEXT: AtomicU32 = AtomicU32::new(0);

impl Fixture {
    fn new(name: &str) -> Self {
        let unique = format!(
            "canopy-test-{}-{}-{name}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let base = std::env::temp_dir().join(unique);
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("root")).expect("create fixture");
        let root = db::normalize_root(&base.join("root").to_string_lossy());
        let conn = db::open(&base.join("index.db")).expect("open index");
        Self { base, root, conn }
    }

    fn path(&self, rel: &str) -> PathBuf {
        Path::new(&self.root).join(rel)
    }

    /// Creates (or overwrites) a file of exactly `len` bytes, creating parent folders.
    fn write(&self, rel: &str, len: usize) {
        let path = self.path(rel);
        fs::create_dir_all(path.parent().unwrap()).expect("create parent");
        fs::write(path, vec![7u8; len]).expect("write file");
    }

    fn full(&self) -> Summary {
        run(|ctl| scan::full_scan(&self.conn, &self.root, ctl)).expect("full scan")
    }

    /// Children of the root, largest first, compared against snapshot `baseline`.
    fn top(&self, baseline: i64) -> Vec<query::Row> {
        query::children(&self.conn, ROOT_ID, "size", true, 0, 100, baseline).expect("children")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Close the index first: Windows will not delete a file that is still open.
        drop(std::mem::replace(
            &mut self.conn,
            Connection::open_in_memory().expect("in-memory db"),
        ));
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn run<T>(f: impl FnOnce(&mut Ctl) -> Result<T, ScanError>) -> Result<T, ScanError> {
    let cancel = AtomicBool::new(false);
    let mut progress = |_: &Progress| {};
    f(&mut Ctl {
        cancel: &cancel,
        progress: &mut progress,
    })
}

/// root/a/x.bin (1000), root/a/b/y.txt (10), root/z.dat (5)
fn sample_tree(fx: &Fixture) {
    fx.write("a/x.bin", 1000);
    fx.write("a/b/y.txt", 10);
    fx.write("z.dat", 5);
}

#[test]
fn first_scan_indexes_the_whole_tree() {
    let fx = Fixture::new("first");
    sample_tree(&fx);

    let s = fx.full();
    assert!(s.first);
    assert_eq!(s.mode, "full");
    assert_eq!((s.files, s.dirs, s.size), (3, 2, 1015));
    assert_eq!(s.errors, 0);

    let rows = fx.top(-1);
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["a", "z.dat"], "children come back largest first");
    let a = &rows[0];
    assert!(a.is_dir);
    assert_eq!(
        (a.size, a.files, a.dirs),
        (1010, 2, 1),
        "folders carry recursive totals"
    );

    let node = query::node(&fx.conn, a.id, -1).unwrap().expect("node exists");
    assert_eq!(node.path, win::join(&fx.root, "a"));
    assert_eq!(node.crumbs.len(), 2);
    assert_eq!(node.child_count, 2);
}

#[test]
fn rescanning_an_unchanged_tree_changes_nothing() {
    let fx = Fixture::new("unchanged");
    sample_tree(&fx);
    fx.full();

    let s = fx.full();
    assert!(!s.first);
    assert_eq!(s.changed, 0);
    assert_eq!((s.files, s.dirs, s.size), (3, 2, 1015));
}

#[test]
fn rescan_records_exactly_what_changed() {
    let fx = Fixture::new("diff");
    sample_tree(&fx);
    fx.full();

    fx.write("new.bin", 200); // added
    fx.write("a/x.bin", 3000); // grown by 2000
    fs::remove_file(fx.path("z.dat")).unwrap(); // removed
    fx.write("c/inner.txt", 50); // new folder

    let s = fx.full();
    assert_eq!(
        s.changed, 4,
        "one change per added, resized, removed entry and new folder"
    );
    assert_eq!((s.files, s.dirs, s.size), (4, 3, 3260));

    let snapshot = query::snapshots(&fx.conn).unwrap()[0].id;
    let changes = query::changes(&fx.conn, snapshot, "", 100).unwrap();
    assert_eq!(changes.total, 4);
    assert_eq!(changes.grown, 200 + 2000 + 50);
    assert_eq!(changes.shrunk, 5);
    let kind_of = |name: &str| {
        changes
            .events
            .iter()
            .find(|e| e.path.ends_with(name))
            .map(|e| (e.kind, e.is_dir))
    };
    assert_eq!(kind_of("new.bin"), Some((1, false)));
    assert_eq!(kind_of("z.dat"), Some((2, false)));
    assert_eq!(kind_of("x.bin"), Some((3, false)));
    assert_eq!(
        kind_of("\\c"),
        Some((1, true)),
        "a new folder is one event, not one per file inside"
    );

    // The folder that grew remembers its size at the start of the snapshot.
    let rows = fx.top(snapshot);
    let a = rows.iter().find(|r| r.name == "a").unwrap();
    assert_eq!((a.size, a.old_size), (3010, Some(1010)));

    // Scoping changes to a sub-folder only reports what lies below it.
    let scoped = query::changes(&fx.conn, snapshot, &win::join(&fx.root, "a"), 100).unwrap();
    assert_eq!(scoped.total, 1);
}

#[test]
fn removing_a_folder_removes_its_subtree() {
    let fx = Fixture::new("remove");
    sample_tree(&fx);
    fx.full();

    fs::remove_dir_all(fx.path("a")).unwrap();
    let s = fx.full();
    assert_eq!(s.changed, 1);
    assert_eq!((s.files, s.dirs, s.size), (1, 0, 5));
    let left: i64 = fx
        .conn
        .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 2, "only the root and z.dat remain in the index");
}

#[test]
fn renamed_folder_keeps_its_rows() {
    let fx = Fixture::new("rename");
    sample_tree(&fx);
    fx.full();
    let before = fx.top(-1).into_iter().find(|r| r.name == "a").unwrap();
    let frn: i64 = fx
        .conn
        .query_row("SELECT frn FROM entries WHERE id = ?1", [before.id], |r| r.get(0))
        .unwrap();
    if frn == 0 {
        eprintln!("skipped: this filesystem reports no file ids, so renames cannot be recognised");
        return;
    }

    fs::rename(fx.path("a"), fx.path("renamed")).unwrap();
    let s = fx.full();
    assert_eq!(s.changed, 1);
    let after = fx
        .top(-1)
        .into_iter()
        .find(|r| r.name == "renamed")
        .expect("renamed folder is listed");
    assert_eq!(after.id, before.id, "the folder was renamed in place, not re-indexed");
    assert_eq!((after.size, after.files), (1010, 2));
}

#[test]
fn journal_refresh_agrees_with_a_full_scan() {
    let fx = Fixture::new("journal");
    sample_tree(&fx);
    fx.full();

    fx.write("new.bin", 200);
    fx.write("a/x.bin", 3000);
    fs::remove_file(fx.path("z.dat")).unwrap();
    fx.write("c/deep/inner.txt", 50);
    fs::rename(fx.path("a/b"), fx.path("a/moved")).unwrap();

    let Some(s) = run(|ctl| scan::journal_refresh(&fx.conn, &fx.root, ctl)).expect("journal refresh") else {
        eprintln!("skipped: no readable NTFS change journal for {}", fx.root);
        return;
    };
    assert_eq!(s.mode, "journal");
    assert_eq!((s.files, s.dirs, s.size), (4, 4, 3260));
    assert!(s.listed < 10, "only the touched folders are listed, got {}", s.listed);

    // The diff must leave the index exactly as a full walk would.
    let check = fx.full();
    assert_eq!(check.changed, 0);
    assert_eq!((check.files, check.dirs, check.size), (4, 4, 3260));

    // With nothing new in the tree, another refresh finds nothing.
    let again = run(|ctl| scan::journal_refresh(&fx.conn, &fx.root, ctl))
        .unwrap()
        .expect("journal still available");
    assert_eq!(again.changed, 0);
}

#[test]
fn cancelled_first_scan_leaves_no_half_built_index() {
    let fx = Fixture::new("cancel");
    sample_tree(&fx);

    let cancel = AtomicBool::new(true);
    let mut progress = |_: &Progress| {};
    let mut ctl = Ctl {
        cancel: &cancel,
        progress: &mut progress,
    };
    let result = scan::full_scan(&fx.conn, &fx.root, &mut ctl);
    assert!(matches!(result, Err(ScanError::Cancelled)));
    assert!(!query::overview(&fx.conn).unwrap().complete);

    let s = fx.full();
    assert!(s.first, "the next scan starts over");
    assert_eq!((s.files, s.size), (3, 1015));
}

#[test]
fn scanning_a_missing_root_is_an_error() {
    let fx = Fixture::new("missing");
    let gone = win::join(&fx.root, "does-not-exist");
    let result = run(|ctl| scan::full_scan(&fx.conn, &gone, ctl));
    assert!(matches!(result, Err(ScanError::Root(_))));
}

#[test]
fn list_queries_find_the_right_files() {
    let fx = Fixture::new("queries");
    sample_tree(&fx);
    fx.write("a/photo.JPG", 400);
    fx.write("a/b/second.jpg", 300);
    fx.full();

    let top = query::top_files(&fx.conn, ROOT_ID, 2).unwrap();
    let names: Vec<&str> = top.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["x.bin", "photo.JPG"]);
    assert_eq!(top[0].dir, win::join(&fx.root, "a"));

    let a = fx.top(-1).into_iter().find(|r| r.name == "a").unwrap();
    let b = query::children(&fx.conn, a.id, "name", false, 0, 100, -1)
        .unwrap()
        .into_iter()
        .find(|r| r.name == "b")
        .unwrap();
    let in_b = query::top_files(&fx.conn, b.id, 10).unwrap();
    assert_eq!(in_b.len(), 2, "a sub-folder query stays inside that folder");

    let types = query::types(&fx.conn, ROOT_ID).unwrap();
    let jpg = types
        .iter()
        .find(|t| t.ext == "jpg")
        .expect("extensions are case-folded");
    assert_eq!((jpg.count, jpg.size), (2, 700));
    assert_eq!(types[0].ext, "bin", "types are sorted by size");
    let types_in_b = query::types(&fx.conn, b.id).unwrap();
    assert_eq!(types_in_b.iter().map(|t| t.count).sum::<i64>(), 2);

    let hits = query::search(&fx.conn, "PHOTO", 10).unwrap();
    assert_eq!(hits.len(), 1, "search ignores case");
    assert!(
        query::search(&fx.conn, "%", 10).unwrap().is_empty(),
        "wildcards are matched literally"
    );
}

#[test]
fn treemap_fills_the_canvas_without_spilling() {
    let fx = Fixture::new("treemap");
    sample_tree(&fx);
    fx.write("big.iso", 4000);
    fx.full();

    let (w, h) = (800.0f32, 400.0f32);
    let rects = query::treemap(&fx.conn, ROOT_ID, w as f64, h as f64, false).unwrap();
    assert!(!rects.is_empty());
    for r in &rects {
        assert!(
            r.x >= -0.01 && r.y >= -0.01 && r.x + r.w <= w + 0.01 && r.y + r.h <= h + 0.01,
            "{r:?} leaves the canvas"
        );
        if r.p >= 0 {
            let p = &rects[r.p as usize];
            assert!(
                r.x >= p.x - 0.01
                    && r.y >= p.y - 0.01
                    && r.x + r.w <= p.x + p.w + 0.01
                    && r.y + r.h <= p.y + p.h + 0.01
            );
            assert_eq!(r.depth, p.depth + 1);
        }
    }
    let top_area: f32 = rects.iter().filter(|r| r.p < 0).map(|r| r.w * r.h).sum();
    assert!(
        (top_area - w * h).abs() < w * h * 0.01,
        "top-level blocks cover the canvas, got {top_area}"
    );
    let biggest = rects
        .iter()
        .filter(|r| r.p < 0)
        .max_by(|a, b| (a.w * a.h).total_cmp(&(b.w * b.h)))
        .unwrap();
    assert_eq!(biggest.name, "big.iso", "area follows size");
}
