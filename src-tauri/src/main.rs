// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Canopy desktop shell: exposes the engine to the web UI as Tauri commands.

use canopy_core::rusqlite::{self, Connection};
use canopy_core::{db, query, scan, win};
use serde::Serialize;
use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};

/// One indexed root: its database and the state of any running scan.
struct RootHandle {
    key: String,
    path: String,
    db_path: PathBuf,
    /// A few read connections so a slow query never blocks tree navigation.
    readers: Vec<Mutex<Connection>>,
    scanning: AtomicBool,
    cancel: AtomicBool,
}

impl RootHandle {
    /// Creates the index file if it does not exist yet and opens the read connections.
    fn open(path: &str, db_path: PathBuf) -> Result<Arc<Self>, String> {
        let conn = db::open(&db_path).map_err(err)?;
        db::meta_set(&conn, "root", path).map_err(err)?;
        drop(conn);
        let mut readers = Vec::new();
        for _ in 0..4 {
            readers.push(Mutex::new(db::open_reader(&db_path).map_err(err)?));
        }
        Ok(Arc::new(Self {
            key: db::root_key(path),
            path: path.to_string(),
            db_path,
            readers,
            scanning: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
        }))
    }

    /// Runs a query on whichever read connection is free. If all of them are
    /// busy, waits for the first.
    fn read<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
        for reader in &self.readers {
            if let Ok(conn) = reader.try_lock() {
                return f(&conn).map_err(err);
            }
        }
        let conn = self.readers[0].lock().unwrap_or_else(|e| e.into_inner());
        f(&conn).map_err(err)
    }
}

/// Tauri commands report errors to the UI as plain strings.
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// What all commands share: the open roots, keyed by `db::root_key`.
#[derive(Default)]
struct AppState {
    roots: Mutex<HashMap<String, Arc<RootHandle>>>,
}

impl AppState {
    /// Reopens every index left behind by earlier sessions.
    fn load() -> Self {
        let state = AppState::default();
        if let Ok(dir) = std::fs::read_dir(db::index_dir()) {
            for entry in dir.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("db") {
                    continue;
                }
                let root = Connection::open(&path).ok().and_then(|c| db::meta_get(&c, "root"));
                if let Some(root) = root {
                    if let Ok(handle) = RootHandle::open(&root, path) {
                        state.roots.lock().unwrap().insert(handle.key.clone(), handle);
                    }
                }
            }
        }
        state
    }

    /// Looks a root up. Fails if it has been removed in the meantime.
    fn get(&self, key: &str) -> Result<Arc<RootHandle>, String> {
        self.roots
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or_else(|| "this folder is no longer indexed".to_string())
    }
}

/// `C:\Users\me` -> `C:\`
fn volume_root(path: &str) -> Option<String> {
    let b = path.as_bytes();
    (b.len() >= 2 && b[1] == b':').then(|| format!("{}:\\", b[0] as char))
}

/// What the sidebar shows for an indexed root.
#[derive(Serialize)]
struct RootInfo {
    key: String,
    path: String,
    scanning: bool,
    #[serde(flatten)]
    overview: query::Overview,
    /// The volume has a change journal we can read, so refreshes are diffs.
    journal: bool,
    fs: String,
    label: String,
    vol_total: u64,
    vol_free: u64,
}

/// Collects the current numbers of a root.
fn root_info(h: &RootHandle) -> RootInfo {
    let overview = h.read(query::overview).unwrap_or_default();
    let vol = volume_root(&h.path)
        .and_then(|v| win::volume_info(&v))
        .unwrap_or_default();
    RootInfo {
        key: h.key.clone(),
        path: h.path.clone(),
        scanning: h.scanning.load(Ordering::Relaxed),
        overview,
        journal: scan::journal_available(&h.path),
        fs: vol.fs,
        label: vol.label,
        vol_total: vol.total,
        vol_free: vol.free,
    }
}

// ---------------------------------------------------------------- commands
//
// Every function marked `#[tauri::command(async)]` can be called from the UI as
// `invoke("name", { args })`. `async` means "run on a worker thread", so a slow
// query never freezes the window. Rust parameter names in snake_case appear in
// JavaScript as camelCase.

/// All indexed roots, sorted by path.
#[tauri::command(async)]
fn list_roots(state: State<'_, AppState>) -> Vec<RootInfo> {
    let handles: Vec<Arc<RootHandle>> = state.roots.lock().unwrap().values().cloned().collect();
    let mut roots: Vec<RootInfo> = handles.iter().map(|h| root_info(h)).collect();
    roots.sort_by_key(|r| r.path.to_lowercase());
    roots
}

/// A mounted drive, for the "This PC" list.
#[derive(Serialize)]
struct DriveInfo {
    path: String,
    label: String,
    fs: String,
    kind: u32,
    total: u64,
    free: u64,
}

/// All drive letters that currently hold a readable volume.
#[tauri::command(async)]
fn list_drives() -> Vec<DriveInfo> {
    win::drives()
        .into_iter()
        .filter_map(|path| {
            let v = win::volume_info(&path)?;
            (v.total > 0).then_some(DriveInfo {
                path,
                label: v.label,
                fs: v.fs,
                kind: v.kind,
                total: v.total,
                free: v.free,
            })
        })
        .collect()
}

/// Shows the native folder picker. `None` when the user cancels.
#[tauri::command(async)]
fn pick_folder() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("Choose a folder to index")
        .pick_folder()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Registers a folder as a root and creates its (still empty) index. The UI
/// starts the first scan afterwards.
#[tauri::command(async)]
fn add_root(state: State<'_, AppState>, path: String) -> Result<RootInfo, String> {
    let root = db::normalize_root(&path);
    let (_, attrs, _) = win::stat_dir(&root).map_err(|e| format!("Cannot open {root} (Windows error {e})"))?;
    if attrs & win::ATTR_DIRECTORY == 0 {
        return Err(format!("{root} is not a folder"));
    }
    let key = db::root_key(&root);
    if let Ok(existing) = state.get(&key) {
        return Ok(root_info(&existing));
    }
    let handle = RootHandle::open(&root, db::db_path_for(&root))?;
    state.roots.lock().unwrap().insert(key, handle.clone());
    Ok(root_info(&handle))
}

/// Forgets a root and deletes its index file. The indexed folder itself is untouched.
#[tauri::command(async)]
fn remove_root(state: State<'_, AppState>, key: String) -> Result<(), String> {
    let handle = state.get(&key)?;
    if handle.scanning.load(Ordering::Relaxed) {
        return Err("Stop the running scan first".into());
    }
    state.roots.lock().unwrap().remove(&key);
    let db_path = handle.db_path.clone();
    drop(handle);
    // Readers close as the last reference goes away; give in-flight queries a moment.
    for _ in 0..40 {
        if std::fs::remove_file(&db_path).is_ok() || !db_path.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    for suffix in ["-wal", "-shm"] {
        let mut side = db_path.clone().into_os_string();
        side.push(suffix);
        let _ = std::fs::remove_file(side);
    }
    Ok(())
}

/// Payload of the `scan-progress` event.
#[derive(Serialize, Clone)]
struct ProgressEvent {
    key: String,
    phase: &'static str,
    files: u64,
    dirs: u64,
    bytes: u64,
    errors: u64,
    current: String,
}

/// Payload of the `scan-done` event.
#[derive(Serialize, Clone)]
struct DoneEvent {
    key: String,
    ok: bool,
    cancelled: bool,
    error: String,
    summary: Option<scan::Summary>,
}

/// Starts a refresh in the background. Progress and completion arrive as
/// `scan-progress` / `scan-done` events. `full` forces a complete walk.
#[tauri::command(async)]
fn start_scan(app: AppHandle, state: State<'_, AppState>, key: String, full: bool) -> Result<bool, String> {
    let handle = state.get(&key)?;
    // `swap` sets the flag and returns what it was before: one scan per root at a time.
    if handle.scanning.swap(true, Ordering::SeqCst) {
        return Ok(false);
    }
    handle.cancel.store(false, Ordering::SeqCst);
    // The scan gets a thread and a write connection of its own. This command
    // returns immediately; the UI learns the rest from the two events.
    std::thread::spawn(move || {
        let result = db::open(&handle.db_path)
            .map_err(scan::ScanError::Sql)
            .and_then(|conn| {
                let mut progress = |p: &scan::Progress| {
                    let _ = app.emit(
                        "scan-progress",
                        ProgressEvent {
                            key: handle.key.clone(),
                            phase: p.phase,
                            files: p.files,
                            dirs: p.dirs,
                            bytes: p.bytes,
                            errors: p.errors,
                            current: p.current.clone(),
                        },
                    );
                };
                let mut ctl = scan::Ctl {
                    cancel: &handle.cancel,
                    progress: &mut progress,
                };
                scan::refresh(&conn, &handle.path, full, &mut ctl)
            });
        handle.scanning.store(false, Ordering::SeqCst);
        let done = match result {
            Ok(summary) => DoneEvent {
                key: handle.key.clone(),
                ok: true,
                cancelled: false,
                error: String::new(),
                summary: Some(summary),
            },
            Err(e) => DoneEvent {
                key: handle.key.clone(),
                ok: false,
                cancelled: matches!(e, scan::ScanError::Cancelled),
                error: e.to_string(),
                summary: None,
            },
        };
        let _ = app.emit("scan-done", done);
    });
    Ok(true)
}

/// Asks a running scan to stop. It ends with a `scan-done` event marked cancelled.
#[tauri::command(async)]
fn cancel_scan(state: State<'_, AppState>, key: String) -> Result<(), String> {
    state.get(&key)?.cancel.store(true, Ordering::SeqCst);
    Ok(())
}

/// Fresh numbers for a single root.
#[tauri::command(async)]
fn root_overview(state: State<'_, AppState>, key: String) -> Result<RootInfo, String> {
    Ok(root_info(&*state.get(&key)?))
}

// The commands from here down to `changes` are thin wrappers: each runs one
// function of `canopy_core::query` on a read connection of the chosen root.
#[tauri::command(async)]
fn node(state: State<'_, AppState>, key: String, id: i64, baseline: i64) -> Result<Option<query::Node>, String> {
    state.get(&key)?.read(|c| query::node(c, id, baseline))
}

#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
fn children(
    state: State<'_, AppState>,
    key: String,
    parent: i64,
    sort: String,
    desc: bool,
    offset: i64,
    limit: i64,
    baseline: i64,
) -> Result<Vec<query::Row>, String> {
    state
        .get(&key)?
        .read(|c| query::children(c, parent, &sort, desc, offset, limit, baseline))
}

#[tauri::command(async)]
fn treemap(
    state: State<'_, AppState>,
    key: String,
    id: i64,
    width: f64,
    height: f64,
    alloc: bool,
) -> Result<Vec<query::TmRect>, String> {
    state.get(&key)?.read(|c| query::treemap(c, id, width, height, alloc))
}

#[tauri::command(async)]
fn top_files(state: State<'_, AppState>, key: String, id: i64, limit: i64) -> Result<Vec<query::FileHit>, String> {
    state.get(&key)?.read(|c| query::top_files(c, id, limit))
}

#[tauri::command(async)]
fn file_types(state: State<'_, AppState>, key: String, id: i64) -> Result<Vec<query::TypeStat>, String> {
    state.get(&key)?.read(|c| query::types(c, id))
}

#[tauri::command(async)]
fn search(state: State<'_, AppState>, key: String, text: String, limit: i64) -> Result<Vec<query::FileHit>, String> {
    state.get(&key)?.read(|c| query::search(c, &text, limit))
}

#[tauri::command(async)]
fn snapshots(state: State<'_, AppState>, key: String) -> Result<Vec<query::Snapshot>, String> {
    state.get(&key)?.read(query::snapshots)
}

#[tauri::command(async)]
fn changes(
    state: State<'_, AppState>,
    key: String,
    baseline: i64,
    prefix: String,
    limit: i64,
) -> Result<query::Changes, String> {
    state.get(&key)?.read(|c| query::changes(c, baseline, &prefix, limit))
}

/// Opens Explorer with the item selected.
#[tauri::command(async)]
fn reveal(path: String) -> Result<(), String> {
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{path}\""))
        .spawn()
        .map(|_| ())
        .map_err(err)
}

/// Opens a file or folder with its default application.
#[tauri::command(async)]
fn open_path(path: String) -> Result<(), String> {
    if win::shell_execute(None, &path, None) {
        Ok(())
    } else {
        Err(format!("Could not open {path}"))
    }
}

/// Whether Canopy is running with administrator rights.
#[tauri::command(async)]
fn is_elevated() -> bool {
    win::is_elevated()
}

/// Restarts Canopy with administrator rights (lets it read protected folders).
#[tauri::command(async)]
fn relaunch_elevated(app: AppHandle) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(err)?;
    if win::shell_execute(Some("runas"), &exe.to_string_lossy(), None) {
        app.exit(0);
        Ok(())
    } else {
        Err("Elevation was declined".into())
    }
}

/// Entry point: reopen existing indexes, register the commands, show the window.
fn main() {
    win::quiet_errors();
    tauri::Builder::default()
        .manage(AppState::load())
        .invoke_handler(tauri::generate_handler![
            list_roots,
            list_drives,
            pick_folder,
            add_root,
            remove_root,
            start_scan,
            cancel_scan,
            root_overview,
            node,
            children,
            treemap,
            top_files,
            file_types,
            search,
            snapshots,
            changes,
            reveal,
            open_path,
            is_elevated,
            relaunch_elevated,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start Canopy");
}
