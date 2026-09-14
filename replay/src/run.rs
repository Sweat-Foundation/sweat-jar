//! Threaded reconciliation driver: build an account worklist, fan it out across
//! named worker threads, and upsert `ReconRow`s into the `results` table via a
//! writer thread. Resumable — accounts already in `results` are skipped on a
//! later run unless `--force`, so a killed process picks up where it left off.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

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
    pub errored: usize,
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

    let (tx, rx) = mpsc::channel::<ReconRow>();
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
                no_baseline: 0,
                sum_calculated: 0,
                sum_actual: 0,
                over_tolerance: 0,
            };
            for row in rx {
                s.processed += 1;
                s.sum_calculated += row.calculated_total_claim.parse::<u128>().unwrap_or(0);
                // Hard-failed rows (`status` "error:...") carry `actual_total_claim`
                // "0" because `load_user` failed and the on-chain figure is
                // genuinely unavailable, so `sum_actual` under-counts by those
                // users' real claims. They are tallied in `errored` instead.
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
                            eprintln!("reconcile account {account_id} failed: {e:#}");
                            ReconRow {
                                account_id,
                                near_account_id: String::new(),
                                calculated_total_claim: "0".to_string(),
                                actual_total_claim: "0".to_string(),
                                delta: "0".to_string(),
                                rel_delta: 0.0,
                                n_claims: 0,
                                status: format!("error:{e}"),
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
