//! Threaded reconciliation driver: build an account worklist, fan it out across
//! named worker threads, and upsert `ReconRow`s into the `results` table via a
//! writer thread. Resumable — accounts already in `results` are skipped on a
//! later run unless `--force`, so a killed process picks up where it left off.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use sweat_jar_model::data::product::Product;

use crate::db::{self, ingest};
use crate::export;
use crate::reconcile::{reconcile_user, ReconRow};
use crate::snapshot::{DbSnapshotSource, SnapshotSource};

/// Resolved options for a reconciliation run.
pub struct RunOpts {
    pub db: PathBuf,
    /// Export `results` to this CSV once the run completes. `None` skips the
    /// export (the database is still updated) — use `export-csv` later.
    pub out: Option<PathBuf>,
    pub products: PathBuf,
    /// Worker-thread count (already resolved from `None` by the caller).
    pub threads: usize,
    /// `(i, n)`: keep accounts where `account_id % n == i`.
    pub shard: Option<(u64, u64)>,
    pub accounts: Option<PathBuf>,
    pub sample: Option<usize>,
    pub tolerance: f64,
    /// `Some(url)` fetches each account's block-H state from that archival RPC
    /// endpoint; `None` reads the local `snapshots` table.
    pub archival_rpc_url: Option<String>,
    /// Recompute accounts that already have a `results` row (default: skip them).
    pub force: bool,
}

/// Aggregate outcome of a run, accumulated as rows are written.
pub struct RunSummary {
    pub processed: usize,
    pub ok: usize,
    /// `status = "error:.."` — the contract logic itself panicked given this
    /// account's real history. Not retryable; rerunning gives the same result.
    pub errored: usize,
    /// `status = "failure:.."` — the tool didn't get a clean read on this
    /// account (RPC error, an unexpected panic escaping the engine's own
    /// guard). Worth retrying, e.g. after fixing connectivity or getting an
    /// archival API key.
    pub failed: usize,
    pub no_baseline: usize,
    pub sum_calculated: u128,
    pub sum_actual: u128,
    pub over_tolerance: usize,
}

/// Parse a `"i/n"` shard spec; validates `n > 0 && i < n`.
pub fn parse_shard(s: &str) -> Result<(u64, u64)> {
    let (i, n) = s
        .split_once('/')
        .with_context(|| format!("shard spec {s:?} is not `i/n`"))?;
    let i: u64 = i.trim().parse().with_context(|| format!("shard index {i:?}"))?;
    let n: u64 = n.trim().parse().with_context(|| format!("shard count {n:?}"))?;
    anyhow::ensure!(n > 0, "shard count must be > 0");
    anyhow::ensure!(i < n, "shard index {i} must be < count {n}");
    Ok((i, n))
}

/// `12h34m56s` / `3m04s` / `41s` — short enough for a one-line log heartbeat.
fn format_duration(d: Duration) -> String {
    let total = d.as_secs();
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}h{m:02}m{s:02}s")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

fn install_quiet_panic_hook() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let msg = info.to_string();
            if msg.contains("GuestPanic") || msg.contains("mocked_blockchain") {
                return;
            }
            prev(info);
        }));
    });
}

fn build_worklist(opts: &RunOpts) -> Result<Vec<i64>> {
    let conn = db::open_read(&opts.db)?;
    let sql = if opts.force {
        "SELECT backend_account_id FROM accounts ORDER BY backend_account_id"
    } else {
        "SELECT backend_account_id FROM accounts \
         WHERE backend_account_id NOT IN (SELECT backend_account_id FROM results) \
         ORDER BY backend_account_id"
    };
    let mut ids: Vec<i64> = conn.prepare(sql)?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;

    if let Some(path) = &opts.accounts {
        let allow: HashSet<i64> = ingest::read_accounts(path)?.into_iter().collect();
        ids.retain(|id| allow.contains(id));
    }
    if let Some((i, n)) = opts.shard {
        ids.retain(|id| id.rem_euclid(n as i64) as u64 == i);
    }
    if let Some(k) = opts.sample {
        ids.truncate(k);
    }
    Ok(ids)
}

/// Run reconciliation over the worklist, upserting each result into the
/// `results` table (resumable: rerunning skips accounts already there unless
/// `opts.force`), then export to `opts.out` if given.
pub fn run(opts: &RunOpts) -> Result<RunSummary> {
    anyhow::ensure!(opts.threads > 0, "threads must be > 0");

    // `run_timeline` catches contract panics into `error:` rows, but near-sdk's
    // default hook still prints every one (with a "GuestPanic" message) to
    // stderr first — thousands of lines on a real run. Install a process-global
    // hook that swallows exactly those and forwards everything else (a genuine
    // bug in our own code still prints). Set once; harmless if `run` is called
    // again.
    install_quiet_panic_hook();

    // `results` may not exist yet (a db built before this table was added).
    db::schema::init_schema(&db::open_write(&opts.db)?)?;

    let worklist = build_worklist(opts)?;
    let total = worklist.len();

    let products: Arc<Vec<Product>> =
        Arc::new(crate::products::load_products(&opts.products)?);
    let snapshot: Arc<dyn SnapshotSource> = match &opts.archival_rpc_url {
        Some(url) => Arc::new(crate::snapshot::ArchivalRpcSnapshotSource {
            rpc_url: url.clone(),
            jar_contract: crate::products::JAR_CONTRACT.to_string(),
            block_height: crate::parse::H_BLOCK,
            api_key: crate::snapshot::api_key_from_env(),
        }),
        None => Arc::new(DbSnapshotSource::new(&opts.db)),
    };
    let queue = Arc::new(Mutex::new(worklist.into_iter()));

    // Bounded, not `mpsc::channel`: the writer commits one row at a time to
    // DuckDB, so it's normally the slow side and an unbounded channel would
    // just be backpressure-free buffering. Network latency (~0.4s/account
    // against a healthy archival RPC) usually keeps producers slow enough
    // that this never mattered -- until a bad/rate-limited API key made every
    // `get_account` fail instantly, removing that natural throttle: workers
    // then produced rows far faster than the writer could drain them, and an
    // unbounded channel buffered millions of them in RAM until the OOM killer
    // stepped in. A bound a few rows deep per worker forces workers to block
    // on `send` instead, capping memory regardless of why the writer is slow.
    let (tx, rx) = mpsc::sync_channel::<ReconRow>(opts.threads * 16);
    let out_path = opts.out.clone();
    let db_path_for_writer = opts.db.clone();
    let tolerance = opts.tolerance;
    let writer = std::thread::Builder::new()
        .name("replay-writer".to_string())
        .spawn(move || -> Result<RunSummary> {
            let conn = db::open_write(&db_path_for_writer)?;
            let mut upsert = conn
                .prepare(
                    "INSERT INTO results \
                        (backend_account_id, near_account_id, calculated_total_claim, \
                         actual_total_claim, delta, rel_delta, n_claims, status, computed_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
                     ON CONFLICT (backend_account_id) DO UPDATE SET \
                        near_account_id = excluded.near_account_id, \
                        calculated_total_claim = excluded.calculated_total_claim, \
                        actual_total_claim = excluded.actual_total_claim, \
                        delta = excluded.delta, \
                        rel_delta = excluded.rel_delta, \
                        n_claims = excluded.n_claims, \
                        status = excluded.status, \
                        computed_at = excluded.computed_at",
                )
                .context("prepare results upsert")?;
            let mut s = RunSummary {
                processed: 0,
                ok: 0,
                errored: 0,
                failed: 0,
                no_baseline: 0,
                sum_calculated: 0,
                sum_actual: 0,
                over_tolerance: 0,
            };
            // Set REPLAY_PROGRESS_INTERVAL_SECS=0 to disable; default 30s. This
            // is the only routine visibility into an unattended multi-hour/day
            // run (e.g. `docker logs -f`) — everything else is silent until the
            // final summary main() prints after `run()` returns.
            let heartbeat_interval = std::env::var("REPLAY_PROGRESS_INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(30);
            let run_start = Instant::now();
            let mut last_heartbeat = Instant::now();
            for row in rx {
                s.processed += 1;
                s.sum_calculated += row.calculated_total_claim.parse::<u128>().unwrap_or(0);
                // Rows with no calculated total (`error:`/`failure:`) carry
                // `actual_total_claim` "0" only when `load_user` itself failed —
                // otherwise it's the real on-chain figure, still meaningful for
                // sum_actual. Either way these are tallied in errored/failed
                // instead of ok, so they don't skew sum_calculated/sum_actual's
                // ratio.
                s.sum_actual += row.actual_total_claim.parse::<u128>().unwrap_or(0);
                if row.status == "ok" {
                    s.ok += 1;
                    if row.rel_delta.abs() > tolerance {
                        s.over_tolerance += 1;
                    }
                } else if row.status == "no_baseline" {
                    s.no_baseline += 1;
                } else if row.status.starts_with("error:") {
                    s.errored += 1;
                    // Printed as each one happens, not just counted at the end —
                    // this is what lets you see these live instead of only after
                    // the whole run finishes. `error:` = the contract logic
                    // itself panicked given this account's real history — not
                    // retryable, see reconcile::reconcile_user's doc.
                    eprintln!("ERROR account={} {}", row.account_id, row.status);
                } else if row.status.starts_with("failure:") {
                    s.failed += 1;
                    // `failure:` = the tool didn't get a clean read (RPC error,
                    // an unexpected engine panic) — worth retrying.
                    eprintln!("FAILURE account={} {}", row.account_id, row.status);
                }

                if heartbeat_interval > 0 && last_heartbeat.elapsed() >= Duration::from_secs(heartbeat_interval) {
                    let elapsed = run_start.elapsed();
                    let rate = s.processed as f64 / elapsed.as_secs_f64().max(1.0);
                    let pct = 100.0 * s.processed as f64 / total.max(1) as f64;
                    let eta = if rate > 0.0 {
                        format_duration(Duration::from_secs_f64((total - s.processed) as f64 / rate))
                    } else {
                        "?".to_string()
                    };
                    eprintln!(
                        "PROGRESS {}/{total} ({pct:.1}%) ok={} error={} failure={} no_baseline={} elapsed={} rate={rate:.2}/s eta={eta}",
                        s.processed, s.ok, s.errored, s.failed, s.no_baseline, format_duration(elapsed),
                    );
                    last_heartbeat = Instant::now();
                }

                let computed_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
                upsert
                    .execute(duckdb::params![
                        row.account_id,
                        row.near_account_id,
                        row.calculated_total_claim,
                        row.actual_total_claim,
                        row.delta,
                        row.rel_delta,
                        row.n_claims as i64,
                        row.status,
                        computed_at,
                    ])
                    .with_context(|| format!("upsert result for account {}", row.account_id))?;
            }
            if heartbeat_interval > 0 {
                let processed = s.processed;
                eprintln!(
                    "PROGRESS {processed}/{total} (100.0%) ok={} error={} failure={} no_baseline={} elapsed={} — done",
                    s.ok, s.errored, s.failed, s.no_baseline, format_duration(run_start.elapsed()),
                );
            }
            drop(upsert);
            if let Some(out_path) = &out_path {
                export::export_csv_with(&conn, out_path)?;
            }
            Ok(s)
        })
        .context("spawn writer thread")?;

    let mut handles = Vec::with_capacity(opts.threads);
    for k in 0..opts.threads {
        let queue = Arc::clone(&queue);
        let products = Arc::clone(&products);
        let snapshot = Arc::clone(&snapshot);
        let tx = tx.clone();
        let db_path = opts.db.clone();
        let handle = std::thread::Builder::new()
            .name(format!("replay-worker-{k}"))
            .spawn(move || -> Result<()> {
                let conn = db::open_read(&db_path)?;
                loop {
                    let next = { queue.lock().unwrap().next() };
                    let Some(account_id) = next else { break };
                    let row = match reconcile_user(
                        &conn,
                        account_id,
                        &products,
                        snapshot.as_ref(),
                    ) {
                        Ok(row) => row,
                        Err(e) => {
                            // A DB/IO failure, not a per-account contract
                            // divergence — `failure:`, same as an RPC error.
                            eprintln!("FAILURE account={account_id} failure:{e:#}");
                            ReconRow {
                                account_id,
                                near_account_id: String::new(),
                                calculated_total_claim: "0".to_string(),
                                actual_total_claim: "0".to_string(),
                                delta: "0".to_string(),
                                rel_delta: 0.0,
                                n_claims: 0,
                                status: format!("failure:{e}"),
                            }
                        }
                    };
                    if tx.send(row).is_err() {
                        break;
                    }
                }
                Ok(())
            })
            .with_context(|| format!("spawn worker {k}"))?;
        handles.push(handle);
    }
    drop(tx);

    let mut worker_err = None;
    for h in handles {
        match h.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => worker_err = worker_err.or(Some(e)),
            Err(_) => worker_err = worker_err.or(Some(anyhow::anyhow!("worker thread panicked"))),
        }
    }

    let summary = writer
        .join()
        .map_err(|_| anyhow::anyhow!("writer thread panicked"))??;

    if let Some(e) = worker_err {
        return Err(e);
    }
    Ok(summary)
}
