//! Debug tool: bisect where one account's replay first disagrees with real
//! archival state, event by event.
//!
//! For each event in the account's timeline, replays up to and including
//! that event, then compares the resulting in-memory account view against a
//! live `get_account` at that event's own on-chain block height (NEAR's
//! view-at-height semantics mean that block already reflects the event).
//! Prints the index of the first event where they diverge, and a JSON diff
//! of the two views at that point.
//!
//! Usage: `bisect --db <db> --account <id> --products <products.json>`
//! (always uses the archival RPC — there's no point bisecting against the
//! local `snapshots` cache, which isn't authoritative).

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use sweat_jar::replay::engine;
use sweat_jar_model::data::account::{versioned::AccountVersioned, view::AccountView};

use replay::db;
use replay::parse;
use replay::products::{self, JAR_CONTRACT};
use replay::reconcile::apply_block_height_fallback;
use replay::snapshot::{ArchivalRpcSnapshotSource, SnapshotSource};
use replay::timeline::load_user;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    db: PathBuf,
    #[arg(long)]
    account: i64,
    #[arg(long, default_value = "test_data/products.json")]
    products: PathBuf,
    #[arg(long, default_value = replay::snapshot::FASTNEAR_ARCHIVAL_RPC)]
    archival_rpc_url: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let conn = db::open_read(&args.db)?;
    let products = products::load_products(&args.products)?;
    let (slice, timeline) = load_user(&conn, args.account)?;

    let snapshot = ArchivalRpcSnapshotSource {
        rpc_url: args.archival_rpc_url.clone(),
        jar_contract: JAR_CONTRACT.to_string(),
        block_height: parse::H_BLOCK,
        api_key: replay::snapshot::api_key_from_env(),
    };

    // Per-event block_height, in the same (ts_ms, log_index) order the
    // timeline is sorted in — needed to know which real block to compare
    // each replayed step against.
    let mut stmt = conn.prepare(
        "SELECT ts_ms, log_index, block_height FROM events \
         WHERE backend_account_id = ? ORDER BY ts_ms, log_index",
    )?;
    let block_heights: std::collections::HashMap<(u64, u64), u64> = stmt
        .query_map(duckdb::params![args.account], |r| {
            Ok(((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64), r.get::<_, i64>(2)? as u64))
        })?
        .collect::<duckdb::Result<_>>()?;

    let raw_account = snapshot.raw_account(args.account, &slice.near_account_id)?;
    let (raw_account, timeline, _claimed_adjustment) =
        apply_block_height_fallback(&snapshot, &slice, raw_account, timeline);

    let account_id: near_sdk::AccountId =
        slice.near_account_id.parse().context("invalid near_account_id")?;

    // Recover each event's on-chain block height in timeline order (post-
    // apply_block_height_fallback, which may have dropped the first event —
    // recompute the mapping fresh from the (possibly trimmed) timeline).
    let heights: Vec<u64> = timeline
        .events
        .iter()
        .map(|e| {
            *block_heights
                .get(&(e.ts_ms, e.seq))
                .unwrap_or_else(|| panic!("no block_height for event ts={} seq={}", e.ts_ms, e.seq))
        })
        .collect();

    let mut replay_views: Vec<AccountView> = Vec::new();
    let outcome = engine::run_timeline_traced(
        engine::Baseline { account_id, raw_account, timezone_ms: slice.timezone_ms },
        &products,
        parse::H_MS,
        timeline,
        |_index, view| replay_views.push(view),
    );

    println!("account {} ({})", args.account, slice.near_account_id);
    println!("replayed {} events, status {:?}\n", replay_views.len(), outcome.status);

    let mut first_divergence: Option<usize> = None;
    for (i, replay_view) in replay_views.iter().enumerate() {
        let block_height = heights[i];
        // Multiple events can share one block (e.g. a Deposit and an
        // ApplyBooster in the same receipt/block). Archival view-at-height
        // reflects the WHOLE block, not one event within it — comparing an
        // earlier event in a shared block against that height would compare
        // "only this event replayed" against "the whole block's effect",
        // a guaranteed false positive. Only the block's LAST event is a
        // fair comparison point.
        if heights.get(i + 1) == Some(&block_height) {
            println!("[{i:>3}] block {block_height}: shares this block with the next event — skipping");
            continue;
        }
        let chain_view = match fetch_real_view(&snapshot, &slice.near_account_id, block_height)? {
            Some(v) => v,
            None => {
                println!("[{i:>3}] block {block_height}: chain state is null (account not yet created) — skipping");
                continue;
            }
        };
        if &chain_view != replay_view {
            println!("[{i:>3}] block {block_height}: FIRST DIVERGENCE");
            println!("  replay: {}", near_sdk::serde_json::to_string(replay_view)?);
            println!("  chain:  {}", near_sdk::serde_json::to_string(&chain_view)?);
            first_divergence = Some(i);
            break;
        }
        println!("[{i:>3}] block {block_height}: match");
    }

    match first_divergence {
        Some(i) => println!("\nFirst divergence at event index {i} (0-based)."),
        None => println!("\nNo divergence found — replay matches real chain state at every event boundary."),
    }

    Ok(())
}

/// Live `get_account` at `block_height`, decoded straight to an `AccountView`
/// (skipping the borsh round-trip `SnapshotSource::raw_account_at` does —
/// this tool wants the parsed view for direct `PartialEq` comparison, not
/// bytes to feed back into a fresh contract).
fn fetch_real_view(
    snapshot: &ArchivalRpcSnapshotSource,
    near_account_id: &str,
    block_height: u64,
) -> Result<Option<AccountView>> {
    let raw = snapshot.raw_account_at(near_account_id, block_height)?;
    let Some(raw) = raw else { return Ok(None) };
    let versioned: AccountVersioned =
        near_sdk::borsh::from_slice(&raw).context("decode AccountVersioned from archival bytes")?;
    // Custom `BorshDeserialize` on `AccountVersioned` always upgrades V1
    // variants to V2 on the way in (see `versioned.rs`), so this is
    // exhaustive in practice.
    let account = match versioned {
        AccountVersioned::V2(account) => account,
        _ => anyhow::bail!("expected AccountVersioned::V2 after deserialize (auto-upgrade should be exhaustive)"),
    };
    Ok(Some(account.into()))
}
