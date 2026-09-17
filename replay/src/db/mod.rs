//! DuckDB connection helpers for the replay database.

pub mod ingest;
pub mod schema;

use std::path::Path;

use anyhow::Context;
use duckdb::Connection;

/// Open (creating if needed) for building. `threads` PRAGMA left at the
/// DuckDB default (all cores) — `build-db` is a one-shot bulk job.
pub fn open_write(path: &Path) -> anyhow::Result<Connection> {
    Connection::open(path).with_context(|| format!("open_write: {}", path.display()))
}

/// Open an existing database read-only.
pub fn open_read(path: &Path) -> anyhow::Result<Connection> {
    let cfg = duckdb::Config::default().access_mode(duckdb::AccessMode::ReadOnly)?;
    Connection::open_with_flags(path, cfg)
        .with_context(|| format!("open_read: {}", path.display()))
}
