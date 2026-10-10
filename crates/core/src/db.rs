// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

//! SQLite index: one database file per indexed root.

use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The root directory itself is always entry 1.
pub const ROOT_ID: i64 = 1;

/// Tables of one index file. Created on first open; `IF NOT EXISTS` makes
/// running it again harmless.
///
/// * `meta`        key/value settings of this index (root path, journal checkpoint, ...)
/// * `entries`     one row per file or folder, linked to its folder by `parent`
/// * `snapshots`   one row per history bucket
/// * `dir_changes` what a folder's totals were before a bucket changed them
/// * `events`      the individual additions, removals and resizes
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS entries(
    id     INTEGER PRIMARY KEY,
    parent INTEGER NOT NULL,
    name   TEXT    NOT NULL,
    is_dir INTEGER NOT NULL,
    size   INTEGER NOT NULL,   -- logical bytes; folders hold the recursive total
    alloc  INTEGER NOT NULL,   -- bytes allocated on disk; folders hold the recursive total
    files  INTEGER NOT NULL,   -- recursive file count (folders only)
    dirs   INTEGER NOT NULL,   -- recursive folder count (folders only)
    mtime  INTEGER NOT NULL,
    attrs  INTEGER NOT NULL,
    frn    INTEGER NOT NULL    -- NTFS file reference number, links journal records to rows
);
CREATE TABLE IF NOT EXISTS snapshots(
    id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, mode TEXT NOT NULL, ms INTEGER NOT NULL,
    size INTEGER NOT NULL, alloc INTEGER NOT NULL, files INTEGER NOT NULL, dirs INTEGER NOT NULL,
    changed INTEGER NOT NULL
);
-- A folder's totals as they were when history bucket `snap` began.
CREATE TABLE IF NOT EXISTS dir_changes(
    id INTEGER NOT NULL, snap INTEGER NOT NULL,
    old_size INTEGER NOT NULL, old_alloc INTEGER NOT NULL, old_files INTEGER NOT NULL,
    PRIMARY KEY(id, snap)
) WITHOUT ROWID;
-- kind: 1 added, 2 removed, 3 resized
CREATE TABLE IF NOT EXISTS events(
    snap INTEGER NOT NULL, path TEXT NOT NULL, is_dir INTEGER NOT NULL, kind INTEGER NOT NULL,
    old_size INTEGER NOT NULL, new_size INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_events ON events(snap);
";

/// Built after the first bulk load; maintaining them during it would slow it down.
const INDEXES: &str = "
CREATE INDEX IF NOT EXISTS ix_parent ON entries(parent, size DESC);
CREATE INDEX IF NOT EXISTS ix_pdir   ON entries(parent) WHERE is_dir = 1;
CREATE INDEX IF NOT EXISTS ix_frn    ON entries(frn) WHERE is_dir = 1;
CREATE INDEX IF NOT EXISTS ix_big    ON entries(size DESC) WHERE is_dir = 0 AND size >= 1048576;
";

/// How many history buckets each index keeps.
pub const KEEP_SNAPSHOTS: i64 = 400;
/// Refreshes closer together than this share one history bucket.
pub const SNAPSHOT_PERIOD_SECS: i64 = 15 * 60;

/// Opens (and if necessary creates) an index file for reading and writing.
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let c = Connection::open(path)?;
    c.busy_timeout(Duration::from_secs(60))?;
    // journal_mode=WAL      readers are never blocked by the scan that is writing
    // synchronous=NORMAL    sync to disk only at checkpoints: the file cannot be
    //                       corrupted, a power cut merely loses the last refresh
    // temp_store, cache_size (64 MB), mmap_size (1 GB)
    //                       keep sorting and reading in memory
    // wal_autocheckpoint    fold the write-ahead log back in every 20,000 pages
    c.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA temp_store=MEMORY;
         PRAGMA cache_size=-65536;
         PRAGMA mmap_size=1073741824;
         PRAGMA wal_autocheckpoint=20000;",
    )?;
    c.set_prepared_statement_cache_capacity(64);
    c.execute_batch(SCHEMA)?;
    Ok(c)
}

/// A connection for UI queries; it can never write.
pub fn open_reader(path: &Path) -> rusqlite::Result<Connection> {
    let c = open(path)?;
    c.execute_batch("PRAGMA query_only=1; PRAGMA cache_size=-16384;")?;
    Ok(c)
}

/// Creates the lookup indexes:
///
/// * `ix_parent` children of a folder, already sorted by size
/// * `ix_pdir`   sub-folders only, for walking a subtree quickly
/// * `ix_frn`    folder by NTFS reference number, for journal records
/// * `ix_big`    files of 1 MB and more by size, for "largest files"
pub fn create_indexes(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch(INDEXES)
}

/// Throws away the index contents (keeps the file and the root path).
pub fn reset(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch(
        "DROP TABLE IF EXISTS entries;
         DROP TABLE IF EXISTS snapshots;
         DROP TABLE IF EXISTS dir_changes;
         DROP TABLE IF EXISTS events;
         DELETE FROM meta WHERE k <> 'root';",
    )?;
    c.execute_batch(SCHEMA)
}

/// Reads a setting from the `meta` table.
pub fn meta_get(c: &Connection, key: &str) -> Option<String> {
    c.query_row("SELECT CAST(v AS TEXT) FROM meta WHERE k = ?1", [key], |r| {
        r.get::<_, Option<String>>(0)
    })
    .optional()
    .ok()
    .flatten()
    .flatten()
}

/// Reads a numeric setting.
pub fn meta_i64(c: &Connection, key: &str) -> Option<i64> {
    meta_get(c, key).and_then(|v| v.parse().ok())
}

/// Writes a setting, replacing any earlier value.
pub fn meta_set(c: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    c.prepare_cached("INSERT INTO meta(k, v) VALUES(?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v")?
        .execute(params![key, value])?;
    Ok(())
}

/// Full path of an entry, or `None` if it no longer exists.
pub fn path_of(c: &Connection, id: i64) -> rusqlite::Result<Option<String>> {
    let mut stmt = c.prepare_cached("SELECT parent, name FROM entries WHERE id = ?1")?;
    // Follow the parent links up to the root, collecting names on the way, then
    // join them in reverse. The root's name is its full path (`C:\` or `C:\Data`).
    let mut parts: Vec<String> = Vec::new();
    let mut cur = id;
    loop {
        let row: Option<(i64, String)> = stmt.query_row([cur], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        let Some((parent, name)) = row else { return Ok(None) };
        parts.push(name);
        if parent == 0 {
            break;
        }
        cur = parent;
    }
    let mut path = parts.pop().unwrap_or_default();
    while let Some(p) = parts.pop() {
        if !path.ends_with('\\') {
            path.push('\\');
        }
        path.push_str(&p);
    }
    Ok(Some(path))
}

/// Canonical spelling of a root: `c:` -> `C:\`, `C:\Users\` -> `C:\Users`.
pub fn normalize_root(input: &str) -> String {
    let mut p = input.trim().trim_matches('"').replace('/', "\\");
    if let Some(rest) = p.strip_prefix(r"\\?\") {
        p = rest.to_string();
    }
    let b = p.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        p = format!("{}{}", (b[0] as char).to_ascii_uppercase(), &p[1..]);
    }
    while p.len() > 3 && p.ends_with('\\') {
        p.pop();
    }
    if p.len() == 2 && p.ends_with(':') {
        p.push('\\');
    }
    p
}

/// Stable, filesystem-safe key for a root path (also the database file stem).
pub fn root_key(root: &str) -> String {
    let lower = root.to_lowercase();
    // A hash keeps the key unique for paths that share their first characters.
    // FNV-1a is used because it is tiny and gives the same value on every run and
    // in every version, which Rust's built-in hasher does not promise.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a
    for b in lower.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let readable: String = lower.chars().filter(|c| c.is_ascii_alphanumeric()).take(24).collect();
    format!("{readable}-{hash:016x}")
}

/// `%LOCALAPPDATA%\Canopy\index`, created on demand.
pub fn index_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("Canopy").join("index");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Where the index file of a root lives.
pub fn db_path_for(root: &str) -> PathBuf {
    index_dir().join(format!("{}.db", root_key(root)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_are_normalized_to_one_spelling() {
        assert_eq!(normalize_root("c:"), r"C:\");
        assert_eq!(normalize_root("c:/"), r"C:\");
        assert_eq!(normalize_root(r"C:\Users\"), r"C:\Users");
        assert_eq!(normalize_root("d:/data/photos/"), r"D:\data\photos");
        assert_eq!(normalize_root(r#"  "C:\Program Files"  "#), r"C:\Program Files");
        assert_eq!(normalize_root(r"\\?\C:\Temp"), r"C:\Temp");
        assert_eq!(normalize_root(r"\\server\share\dir\"), r"\\server\share\dir");
    }

    #[test]
    fn root_keys_are_stable_and_filesystem_safe() {
        let key = root_key(r"C:\Users\Me");
        assert_eq!(key, root_key(r"c:\users\me"), "keys ignore case, like the filesystem");
        assert_ne!(key, root_key(r"C:\Users\You"));
        assert!(key.starts_with("cusersme-"));
        assert!(key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    #[test]
    fn meta_values_round_trip() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        assert_eq!(meta_get(&c, "missing"), None);
        meta_set(&c, "answer", "42").unwrap();
        meta_set(&c, "answer", "43").unwrap();
        assert_eq!(meta_get(&c, "answer").as_deref(), Some("43"));
        assert_eq!(meta_i64(&c, "answer"), Some(43));
    }

    #[test]
    fn paths_are_rebuilt_from_parent_links() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        let insert = |id: i64, parent: i64, name: &str| {
            c.execute(
                "INSERT INTO entries(id, parent, name, is_dir, size, alloc, files, dirs, mtime, attrs, frn)
                 VALUES(?1, ?2, ?3, 1, 0, 0, 0, 0, 0, 0, 0)",
                params![id, parent, name],
            )
            .unwrap();
        };
        insert(ROOT_ID, 0, r"C:\");
        insert(2, ROOT_ID, "Users");
        insert(3, 2, "Me");
        assert_eq!(path_of(&c, ROOT_ID).unwrap().as_deref(), Some(r"C:\"));
        assert_eq!(
            path_of(&c, 3).unwrap().as_deref(),
            Some(r"C:\Users\Me"),
            "no doubled separator after a drive root"
        );
        assert_eq!(path_of(&c, 99).unwrap(), None);
    }

    #[test]
    fn reset_empties_the_index_but_keeps_the_root() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        meta_set(&c, "root", r"C:\").unwrap();
        meta_set(&c, "complete", "1").unwrap();
        reset(&c).unwrap();
        assert_eq!(meta_get(&c, "root").as_deref(), Some(r"C:\"));
        assert_eq!(meta_get(&c, "complete"), None);
        create_indexes(&c).unwrap();
    }
}
