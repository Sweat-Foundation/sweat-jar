//! `export-csv` — dump the `results` table (populated by `run`) to a CSV file.
//!
//! Column order and names match the historical `reconciliation.csv` shape
//! (`reconcile::ReconRow`), so existing consumers of the CSV are unaffected by
//! results now living in the database first.

use std::path::Path;

use anyhow::{Context, Result};

use crate::db;

/// `SELECT` side of the export — shared with `run`'s own end-of-run export so
/// the two never drift apart.
pub const RESULTS_AS_CSV_ROWS: &str = "\
    SELECT backend_account_id AS account_id, near_account_id, calculated_total_claim, \
           actual_total_claim, delta, rel_delta, n_claims, status \
    FROM results ORDER BY backend_account_id";

/// Writes every row currently in `results` to `out_path` as CSV (header included).
pub fn export_csv(db_path: &Path, out_path: &Path) -> Result<()> {
    let conn = db::open_read(db_path)?;
    export_csv_with(&conn, out_path)
}

/// Same as [`export_csv`] but reuses an existing connection (the caller may
/// already hold a write connection, e.g. `run`'s writer thread).
pub fn export_csv_with(conn: &duckdb::Connection, out_path: &Path) -> Result<()> {
    let out = out_path.display().to_string().replace('\'', "''");
    conn.execute_batch(&format!("COPY ({RESULTS_AS_CSV_ROWS}) TO '{out}' (FORMAT csv, HEADER)"))
        .with_context(|| format!("export results to {}", out_path.display()))
}
