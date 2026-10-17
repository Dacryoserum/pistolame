//! Times a first import, a rescan without changes and a snapshot of a fresh library built from
//! the given folders. The music files are only read; the database is a new file.
//!
//! Usage: `cargo run --release -p pistolame-library --example scan_benchmark -- <new database> <folder>...`
#![allow(clippy::print_stdout, reason = "command-line measurement tool")]

use std::path::PathBuf;
use std::time::Instant;

use pistolame_library::LibraryStore;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1).map(PathBuf::from);
    let database = arguments.next().ok_or("missing <new database>")?;
    let roots: Vec<PathBuf> = arguments.collect();
    if roots.is_empty() {
        return Err("missing <folder>".into());
    }
    if database.exists() {
        return Err(format!("{} already exists: give a new file", database.display()).into());
    }
    let mut store = LibraryStore::open(&database)?;
    store.add_roots(&roots)?;
    let started = Instant::now();
    let report = store.synchronize()?;
    let first_import = started.elapsed();
    let started = Instant::now();
    let rescan = store.synchronize()?;
    let unchanged_rescan = started.elapsed();
    let started = Instant::now();
    let snapshot = store.snapshot()?;
    let snapshot_time = started.elapsed();
    println!(
        "{} tracks, {} errors",
        snapshot.tracks.len(),
        report.errors.len()
    );
    println!("first import:     {first_import:?}");
    println!(
        "unchanged rescan: {unchanged_rescan:?} (changed: {})",
        rescan.has_changed
    );
    println!("snapshot:         {snapshot_time:?}");
    Ok(())
}
