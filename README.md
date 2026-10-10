# Canopy

Disk usage for Windows that you only have to compute once. Canopy indexes a drive
or folder into a SQLite database, then keeps that index current by reading only
what changed.

## How it stays fast

| Step | What happens |
| --- | --- |
| First index | Worker threads list folders with `GetFileInformationByHandleEx(FileIdBothDirectoryInfo)`: one kernel call returns a buffer full of entries, each with logical size, allocated size and NTFS file id. No per-file `stat`. |
| Opening the app | Folder totals are stored in the index, so every view is an indexed lookup. Nothing is rescanned to show the tree. |
| Refresh on NTFS | Canopy reads the volume's USN change journal from its last checkpoint, re-lists only the folders named there and pushes the size differences up the tree. No administrator rights needed. |
| Refresh elsewhere | Network shares, exFAT and volumes without a journal get a full parallel walk that still writes only the differences. |

Because every refresh is a diff, Canopy also knows *what* changed: the Changes tab
and the Change column show what grew or shrank since any earlier point.

## Layout

- `crates/core` — the engine (`win.rs` Win32 layer, `scan.rs` scan and diff, `db.rs` schema, `query.rs` read side) plus `canopy-cli`
- `src-tauri` — the desktop shell (Tauri 2, WebView2)
- `ui` — the interface: plain HTML, CSS and JavaScript modules, no build step and no framework
  - `js/main.js` entry point and global event wiring
  - `js/state.js`, `js/format.js`, `js/filetypes.js`, `js/api.js`, `js/ui.js` shared state and helpers
  - `js/sidebar.js`, `js/scan.js`, `js/nav.js` roots, scanning and navigation
  - `js/tree.js`, `js/treemap.js`, `js/tabs.js`, `js/search.js` the views

## Build and run

Needs Rust (stable, MSVC toolchain) and the WebView2 runtime that ships with Windows 11.

```
cargo build --release
target\release\canopy.exe
```

The UI is embedded at compile time. After editing files in `ui`, touch
`src-tauri/build.rs` (or any source file there) before rebuilding.

Headless use:

```
target\release\canopy-cli.exe scan C:\            index, or refresh by diff
target\release\canopy-cli.exe scan C:\ --full     force a full walk
target\release\canopy-cli.exe ls C:\              largest children of the root
target\release\canopy-cli.exe top C:\             largest files
target\release\canopy-cli.exe changes C:\         what recent refreshes found
target\release\canopy-cli.exe journal C           probe the change journal
```

Indexes live in `%LOCALAPPDATA%\Canopy\index`, one file per root. They are caches:
deleting one only means the next scan starts from scratch.

## Known limits

- Junctions and symbolic links are listed but not followed.
- Hard-linked files are counted once per link.
- Folders the current user may not read are skipped; run as administrator to include them.
- Sizes of files that are open for writing can lag until the writer closes them.

## Tests

```
cargo test -p canopy-core
```

Unit tests cover path handling, the treemap layout and the Win32 directory
reader. The integration tests in `crates/core/tests` build small directory trees
in the temp folder and check the index against them, including that a change-journal
refresh leaves the index exactly as a full rescan would.

## Disclaimer

Canopy is a personal project and comes with no warranty of any kind. **You use it
entirely at your own risk.** It never modifies or deletes your files; it writes only its own index and
settings, but the author accepts no liability for data loss, wrong
figures or any other damage arising from its use. Do not delete files based on
its output without checking for yourself what they are.

## License

MIT, see [LICENSE](LICENSE). Copyright (c) 2026 Dr.-Ing. Axel Schild.
