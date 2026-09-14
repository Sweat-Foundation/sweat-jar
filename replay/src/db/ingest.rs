//! Event-export loaders for the replay DuckDB database.
//!
//! The real `build_db` is implemented in Task 3 of
//! `docs/superpowers/plans/2026-09-10-event-sourced-replay.md`.

use std::path::Path;

use anyhow::{Context, Result};
use duckdb::Connection;

use crate::parse::{H_MS, T_END_MS};

/// Options controlling which accounts are ingested.
pub struct BuildOpts<'a> {
    pub source_dir: &'a Path,
    pub accounts: Option<&'a std::collections::HashSet<i64>>,
    pub sample: Option<usize>,
}

fn glob(dir: &Path, sub: &str) -> String {
    format!("{}/{sub}/*.parquet", dir.display())
}

/// Ingest the `interest_replay/` parquet export into `conn` and return
/// `(table, rows)` in load order.
pub fn build_db(conn: &mut Connection, opts: &BuildOpts) -> Result<Vec<(String, i64)>> {
    let acc_glob = glob(opts.source_dir, "accounts");
    let tz_glob = glob(opts.source_dir, "account_timezones");
    let ev_glob = glob(opts.source_dir, "events");

    // 1. accounts (+ timezone join), optionally filtered/sampled.
    let where_acc = match opts.accounts {
        Some(set) => {
            // `IN ()` is a DuckDB parser error; refuse before building the SQL.
            anyhow::ensure!(!set.is_empty(), "account filter is empty: nothing to ingest");
            let list = set.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
            format!("WHERE a.backend_account_id IN ({list})")
        }
        None => String::new(),
    };
    let limit = opts.sample.map(|n| format!("LIMIT {n}")).unwrap_or_default();
    conn.execute_batch(&format!(
        "INSERT INTO accounts
         SELECT a.backend_account_id, a.near_account_id, a.existed_at_start, t.timezone_ms
         FROM read_parquet('{acc_glob}') a
         LEFT JOIN read_parquet('{tz_glob}') t USING (backend_account_id)
         {where_acc}
         ORDER BY a.backend_account_id
         {limit};"
    ))
    .context("insert accounts")?;

    // 2. events for those accounts only, success rows, sorted.
    conn.execute_batch(&format!(
        "INSERT INTO events
         SELECT e.backend_account_id,
                epoch_ms(e.block_timestamp_utc) AS ts_ms,
                e.log_index,
                e.event,
                e.role,
                e.payload
         FROM read_parquet('{ev_glob}') e
         SEMI JOIN accounts USING (backend_account_id)
         WHERE e.receipt_status = 'SUCCESS_VALUE'
         ORDER BY e.backend_account_id, ts_ms, e.log_index;"
    ))
    .context("insert events")?;

    // 3. meta + row counts.
    let put = |k: &str, v: String| -> Result<()> {
        conn.execute(
            "INSERT INTO meta VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            duckdb::params![k, v],
        )
        .map(|_| ())
        .context("meta")
    };
    put("source_dir", opts.source_dir.display().to_string())?;
    put("h_ms", H_MS.to_string())?;
    put("t_end_ms", T_END_MS.to_string())?;

    let count = |t: &str| -> Result<i64> {
        conn.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
            .context("count")
    };
    let counts = vec![
        ("accounts".to_string(), count("accounts")?),
        ("events".to_string(), count("events")?),
    ];
    for (t, n) in &counts {
        put(&format!("{t}_rows"), n.to_string())?;
    }
    Ok(counts)
}

/// Parse an account-id list file: one `account_id` per line, blank lines and
/// `#` comments ignored. Shared by `build-db --accounts` and `run --accounts`.
///
/// Errors on a file with no ids: an empty filter would otherwise become an
/// `IN ()` clause (a DuckDB parser error) or a silent empty run.
pub fn read_accounts(path: &Path) -> anyhow::Result<Vec<i64>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read accounts file {}", path.display()))?;
    let ids = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.parse::<i64>().with_context(|| format!("parse account_id {l:?}")))
        .collect::<anyhow::Result<Vec<i64>>>()?;
    anyhow::ensure!(
        !ids.is_empty(),
        "--accounts file contains no account ids: {}",
        path.display()
    );
    Ok(ids)
}
