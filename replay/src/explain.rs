//! `explain` — replay one account and print its per-claim breakdown next to the
//! on-chain claim amounts, so a non-zero `delta` can be traced to the claim that
//! diverged.
//!
//! Rewritten in Task 7 of
//! `docs/superpowers/plans/2026-09-10-event-sourced-replay.md`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use sweat_jar::replay::engine::{self, ReplayStatus};

use crate::db;
use crate::parse;
use crate::products::{self, JAR_CONTRACT};
use crate::snapshot::{ArchivalRpcSnapshotSource, DbSnapshotSource, SnapshotSource};
use crate::timeline::load_user;

pub struct ExplainOpts {
    pub db: PathBuf,
    pub account: i64,
    pub products: PathBuf,
    pub archival_rpc_url: Option<String>,
}

pub fn explain(opts: &ExplainOpts) -> Result<()> {
    let conn = db::open_read(&opts.db)?;
    let products = products::load_products(&opts.products)?;
    let (slice, timeline) = load_user(&conn, opts.account)?;

    let snapshot: Box<dyn SnapshotSource> = match &opts.archival_rpc_url {
        Some(url) => Box::new(ArchivalRpcSnapshotSource {
            rpc_url: url.clone(),
            jar_contract: JAR_CONTRACT.to_string(),
            block_height: parse::H_BLOCK,
            api_key: crate::snapshot::api_key_from_env(),
        }),
        None => Box::new(DbSnapshotSource::new(&opts.db)),
    };

    // Always ask — `existed_at_start` is the export's classification, not
    // ground truth; a live archival lookup is authoritative either way.
    let raw_account = snapshot.raw_account(opts.account, &slice.near_account_id)?;
    let had_baseline = raw_account.is_some();
    let no_baseline = !had_baseline && !snapshot.is_authoritative() && slice.existed_at_start;
    let account_id: near_sdk::AccountId = slice
        .near_account_id
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid near_account_id"))?;

    let outcome = engine::run_timeline(
        engine::Baseline { account_id, raw_account, timezone_ms: slice.timezone_ms },
        &products,
        parse::H_MS,
        timeline,
    );

    // On-chain claim total per timestamp (the events table has one claim row per product).
    let mut stmt = conn.prepare(
        "SELECT ts_ms, payload FROM events WHERE backend_account_id = ?1 AND event = 'claim' ORDER BY ts_ms",
    )?;
    let rows = stmt.query_map(duckdb::params![opts.account], |r| {
        Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?))
    })?;
    let mut onchain: BTreeMap<u64, u128> = BTreeMap::new();
    for row in rows {
        let (ts, payload) = row?;
        if let Some(crate::payload::ParsedEvent::Claim { total }) =
            crate::payload::parse_event("claim", None, &payload)?
        {
            *onchain.entry(ts).or_default() += total;
        }
    }

    let calc: BTreeMap<u64, u128> = outcome.per_claim.iter().copied().collect();

    println!("account {} ({})", opts.account, slice.near_account_id);
    let baseline_label = if no_baseline {
        "MISSING (no_baseline)"
    } else if !had_baseline {
        "none (confirmed no state at H)"
    } else {
        "loaded"
    };
    println!("baseline: {baseline_label}");
    println!("status: {:?}", outcome.status);
    println!();
    println!(
        "{:<16}  {:>28}  {:>28}  {:>28}  {:>10}",
        "claim_ts_ms", "calculated", "on_chain", "delta", "rel"
    );

    let all_ts: std::collections::BTreeSet<u64> =
        calc.keys().chain(onchain.keys()).copied().collect();
    let (mut sum_c, mut sum_o) = (0i128, 0i128);
    for ts in all_ts {
        let c = calc.get(&ts).copied().unwrap_or(0);
        let o = onchain.get(&ts).copied().unwrap_or(0);
        let d = c as i128 - o as i128;
        sum_c += c as i128;
        sum_o += o as i128;
        let rel = if o == 0 { 0.0 } else { d as f64 / o as f64 };
        println!("{ts:<16}  {c:>28}  {o:>28}  {d:>28}  {rel:>10.5}");
    }
    println!();
    let dt = sum_c - sum_o;
    println!(
        "TOTAL  calculated {sum_c}  on_chain {sum_o}  delta {dt}  rel {:.6}",
        if sum_o == 0 { 0.0 } else { dt as f64 / sum_o as f64 }
    );

    if let ReplayStatus::Error(msg) = &outcome.status {
        println!("\nengine error: {msg}");
    }
    Ok(())
}
