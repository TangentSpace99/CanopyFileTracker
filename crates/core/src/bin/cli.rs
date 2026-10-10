// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

//! Headless front end for the engine: handy for benchmarks and scripting.
//!
//!   canopy-cli scan <root> [--full]     index or refresh a root
//!   canopy-cli ls <root> [id]           largest children of a folder
//!   canopy-cli top <root>               largest files
//!   canopy-cli types <root>             file-type breakdown
//!   canopy-cli changes <root>           what the last refresh found
//!   canopy-cli journal <letter>         probe the NTFS change journal

use canopy_core::{db, query, scan, win};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

/// Formats a byte count in binary units, as Explorer does (1 KB = 1024 bytes).
fn human(n: i64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = n as f64;
    let mut u = 0;
    while v.abs() >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.2} {}", UNITS[u])
    }
}

fn main() {
    win::quiet_errors();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let target = args.get(1).cloned().unwrap_or_default();
    if cmd.is_empty() || target.is_empty() {
        eprintln!("usage: canopy-cli <scan|ls|top|types|changes|journal> <root> [--full | id]");
        std::process::exit(2);
    }

    // `journal` takes a drive letter and needs no index, so it is handled before
    // one is opened.
    if cmd == "journal" {
        let letter = target.chars().next().unwrap().to_ascii_uppercase();
        println!("elevated: {}", win::is_elevated());
        match win::Journal::open(letter) {
            Ok(j) => {
                println!(
                    "journal {:#x}  first_usn={}  next_usn={}  privileged={}",
                    j.id, j.first_usn, j.next_usn, j.privileged
                );
                let t = Instant::now();
                let mut n = 0u64;
                let mut zero_parent = 0u64;
                let res = j.read(j.first_usn, |r| {
                    n += 1;
                    if r.parent_frn == 0 {
                        zero_parent += 1;
                    }
                });
                println!(
                    "read whole journal: {res:?}  records={n}  without_parent={zero_parent}  in {:?}",
                    t.elapsed()
                );
            }
            Err(e) => println!("journal unavailable: {e:?}"),
        }
        return;
    }

    // Every other command works on the index of the given root.
    let root = db::normalize_root(&target);
    let conn = db::open(&db::db_path_for(&root)).expect("open index");

    match cmd {
        "scan" => {
            let full = args.iter().any(|a| a == "--full");
            let cancel = AtomicBool::new(false);
            let t = Instant::now();
            let mut progress = |p: &scan::Progress| {
                eprint!(
                    "\r{:<9} {:>9} files {:>8} folders {:>10}  {:.0} files/s      ",
                    p.phase,
                    p.files,
                    p.dirs,
                    human(p.bytes as i64),
                    p.files as f64 / t.elapsed().as_secs_f64().max(0.001)
                );
            };
            let mut ctl = scan::Ctl {
                cancel: &cancel,
                progress: &mut progress,
            };
            match scan::refresh(&conn, &root, full, &mut ctl) {
                Ok(s) => {
                    eprintln!();
                    println!(
                        "{} [{}{}] in {} ms: {} files, {} folders, {} ({} on disk)",
                        root,
                        s.mode,
                        if s.first { ", first" } else { "" },
                        s.ms,
                        s.files,
                        s.dirs,
                        human(s.size),
                        human(s.alloc)
                    );
                    println!(
                        "listed {} folders, {} changes, {} unreadable, {} journal records",
                        s.listed, s.changed, s.errors, s.records
                    );
                }
                Err(e) => {
                    eprintln!();
                    eprintln!("scan failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "ls" => {
            let id = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(db::ROOT_ID);
            let t = Instant::now();
            let rows = query::children(&conn, id, "size", true, 0, 25, -1).expect("query");
            let took = t.elapsed();
            for r in &rows {
                println!(
                    "{:>8} {:>12} {:>12} {:>9} {}{}",
                    r.id,
                    human(r.size),
                    human(r.alloc),
                    r.files,
                    r.name,
                    if r.is_dir { "\\" } else { "" }
                );
            }
            println!("-- {} rows in {took:?}", rows.len());
            let t = Instant::now();
            let tm = query::treemap(&conn, id, 1200.0, 500.0, false).expect("treemap");
            println!("-- treemap: {} rects in {:?}", tm.len(), t.elapsed());
        }
        "top" => {
            let t = Instant::now();
            let rows = query::top_files(&conn, db::ROOT_ID, 20).expect("query");
            let took = t.elapsed();
            for r in &rows {
                println!("{:>12}  {}\\{}", human(r.size), r.dir.trim_end_matches('\\'), r.name);
            }
            println!("-- in {took:?}");
        }
        "types" => {
            let t = Instant::now();
            let rows = query::types(&conn, db::ROOT_ID).expect("query");
            let took = t.elapsed();
            for r in rows.iter().take(20) {
                println!("{:>12} {:>9}  .{}", human(r.size), r.count, r.ext);
            }
            println!("-- {} types in {took:?}", rows.len());
        }
        "changes" => {
            let snaps = query::snapshots(&conn).expect("query");
            for s in snaps.iter().take(5) {
                println!(
                    "snapshot {} [{}] {} ms, {} changes, {}",
                    s.id,
                    s.mode,
                    s.ms,
                    s.changed,
                    human(s.size)
                );
            }
            let base = snaps.first().map_or(0, |s| s.id);
            let ch = query::changes(&conn, base, "", 25).expect("query");
            println!(
                "in bucket {base}: {} events, +{} / -{}",
                ch.total,
                human(ch.grown),
                human(ch.shrunk)
            );
            for e in &ch.events {
                let sign = ["?", "+", "-", "~"][e.kind.clamp(0, 3) as usize];
                println!("  {sign} {:>12}  {}", human(e.new_size - e.old_size), e.path);
            }
        }
        other => {
            eprintln!("unknown command: {other}");
            std::process::exit(2);
        }
    }
}
