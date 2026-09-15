//! Single-user reconciliation: replay a user's timeline and compare the
//! calculated total claim against the on-chain total.

use std::sync::OnceLock;
use std::time::Instant;

use anyhow::{Context, Result};
use sweat_jar::replay::engine::{self, Action, ReplayStatus, Timeline};
use sweat_jar_model::data::product::Product;

use crate::parse;
use crate::snapshot::SnapshotSource;
use crate::timeline::{self, UserSlice};

/// Set `REPLAY_TIMING=1` to print one `TIMING …` line per account to stderr —
/// a load breakdown (snapshot fetch vs. DB load vs. engine replay) for sizing
/// a run before committing to it.
fn timing_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("REPLAY_TIMING").is_some())
}

#[derive(Default)]
struct Timing {
    load_us: u128,
    fetch_us: u128,
    replay_us: u128,
}

impl Timing {
    fn log(&self, account_id: i64, status: &str) {
        if timing_enabled() {
            let total = self.load_us + self.fetch_us + self.replay_us;
            eprintln!(
                "TIMING account={account_id} status={status} load_us={} fetch_us={} replay_us={} total_us={total}",
                self.load_us, self.fetch_us, self.replay_us,
            );
        }
    }
}

/// One reconciliation result row.
#[derive(Debug, serde::Serialize)]
pub struct ReconRow {
    pub account_id: i64,
    pub near_account_id: String,
    pub calculated_total_claim: String,
    pub actual_total_claim: String,
    pub delta: String,
    pub rel_delta: f64,
    pub n_claims: usize,
    pub status: String,
}

fn truncate(s: &str) -> String {
    if s.chars().count() <= 120 {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(120).collect::<String>())
    }
}

/// A row with `calculated = 0` for the error / no-snapshot-yet paths.
fn zero_calc_row(slice: &UserSlice, status: impl Into<String>) -> ReconRow {
    let actual = slice.onchain_claimed;
    let delta: i128 = -i128::try_from(actual).unwrap_or(i128::MAX);
    ReconRow {
        account_id: slice.backend_account_id,
        near_account_id: slice.near_account_id.clone(),
        calculated_total_claim: "0".to_string(),
        actual_total_claim: actual.to_string(),
        delta: delta.to_string(),
        rel_delta: if actual == 0 { 0.0 } else { delta as f64 / actual as f64 },
        n_claims: 0,
        status: status.into(),
    }
}

/// Best-effort `raw_account_at`: a fetch failure or panic is treated as
/// "didn't find anything" rather than propagated — this fallback is always
/// optional, layered on top of an already-established `raw_account`/`no_baseline`
/// path.
fn call_snapshot_safely(
    snapshot: &dyn SnapshotSource,
    near_account_id: &str,
    block_height: u64,
) -> Option<Vec<u8>> {
    let result: std::thread::Result<Result<Option<Vec<u8>>>> =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            snapshot.raw_account_at(near_account_id, block_height)
        }));
    match result {
        Ok(Ok(v)) => v,
        _ => None,
    }
}

/// Confirmed-empty at block H (an authoritative source said so) — but that
/// doesn't mean the account was still empty right before its first *tracked*
/// event. Something invisible to the 7 tracked event types (most commonly an
/// `FtMessage::Migrate` from a previous contract version, which writes
/// storage directly and emits no event at all) can create an account
/// anywhere within the replay window, including well before an otherwise
/// perfectly ordinary `Deposit` that would normally be trusted to be the
/// account's genesis. Two cases, mutually exclusive by the first action's
/// kind (never both attempted for the same account):
///
/// 1. **First action is a `Deposit`: check the state one block before it.**
///    `deposit()` (via `FtMessage::Stake`) completes within a single
///    receipt — no lock, no promise/callback — so state one block earlier is
///    genuinely a separate prior moment, never a mid-flight snapshot of the
///    Deposit itself. If non-empty, the account already had jars nobody
///    among our 7 event types created; use that state as the baseline and
///    replay the Deposit normally on top of it (do NOT drop it).
/// 2. **First action is anything else: the already-shipped post-event
///    fetch.** Restake/Withdraw/WithdrawAll lock jars, send a
///    withdrawal/transfer promise, and only emit their event from the
///    *callback*, one or more blocks later — so "one block before the
///    event's own block" can land mid-operation (e.g. jars still marked
///    `is_pending_withdraw`), a transient snapshot of the very same action,
///    not a separate prior state (confirmed by tracing a real account this
///    way: `is_pending_withdraw: true` on the jars a Restake was about to
///    consume, one block before its own log event). So for a non-`Deposit`
///    first action we only ever check the event's own (post-execution)
///    block height, on the theory that a non-Deposit first action itself
///    proves the account already existed — the invisible creation must have
///    happened at or before this event, and NEAR's view-at-height semantics
///    return state as of right after this block's receipts ran, already
///    reflecting it. This leading event is then dropped from the timeline
///    before replaying the rest.
///
/// Returns the (possibly updated) baseline, (possibly trimmed) timeline, and
/// an `onchain_claimed` adjustment: when case 2 drops a leading `Claim`, its
/// on-chain amount must come out of the account's total too — otherwise it
/// counts on the on-chain side but was never replayed on the calculated
/// side, producing a spurious divergence rather than a fixed reconciliation.
/// All three are unchanged when neither case applies.
pub fn apply_block_height_fallback(
    snapshot: &dyn SnapshotSource,
    slice: &UserSlice,
    raw_account: Option<Vec<u8>>,
    mut timeline: Timeline,
) -> (Option<Vec<u8>>, Timeline, u128) {
    if raw_account.is_some() || !snapshot.is_authoritative() {
        return (raw_account, timeline, 0);
    }
    let Some(block_height) = slice.first_event_block_height else {
        return (raw_account, timeline, 0);
    };
    let first_is_deposit =
        matches!(timeline.events.first(), Some(e) if matches!(e.action, Action::Deposit { .. }));

    // Case 1: Deposit is single-receipt/atomic, so the block right before it
    // is always safe to check.
    if first_is_deposit {
        return match call_snapshot_safely(snapshot, &slice.near_account_id, block_height.saturating_sub(1)) {
            Some(bytes) => (Some(bytes), timeline, 0),
            None => (raw_account, timeline, 0),
        };
    }

    // Case 2: any other first action may be a multi-receipt operation (lock
    // + promise + callback) — only the event's own (post-execution) block is
    // ever safe to check.
    match call_snapshot_safely(snapshot, &slice.near_account_id, block_height) {
        Some(bytes) => {
            let dropped = timeline.events.remove(0);
            let claimed_adjustment = if matches!(dropped.action, Action::Claim) {
                slice.first_event_claim_amount.unwrap_or(0)
            } else {
                0
            };
            (Some(bytes), timeline, claimed_adjustment)
        }
        None => (raw_account, timeline, 0),
    }
}

/// Reconcile one user. Never panics — a bad account produces a row, not a
/// crash. `status` is one of:
/// - `ok` / `no_baseline` — succeeded.
/// - `error:<msg>` — the replay ran cleanly and the *contract logic itself*
///   panicked given this account's real history (e.g. "Timezone is not set",
///   "Account … is not found", "Not enough funds to restake"). Rerunning
///   won't change these — see `replay/README.md`'s "Known error causes" for
///   what each one means and whether it's expected.
/// - `failure:<msg>` — the tool didn't get a clean read on this account:
///   the archival RPC fetch errored/panicked, or `run_timeline` itself was
///   escaped by an unexpected panic (a genuine bug, not a modeled contract
///   panic). Worth retrying — `run --force --accounts <ids>` on just the
///   `failure:` rows after investigating (or after getting/rotating an
///   archival API key, if these are RPC timeouts).
///
/// Returns `Err` only for a DB/IO failure that isn't user-specific.
///
/// The `catch_unwind` around `run_timeline` is load-bearing, not
/// belt-and-suspenders: `run_timeline`'s own guard has been observed to leak a
/// second panic on the same thread.
pub fn reconcile_user(
    conn: &duckdb::Connection,
    backend_account_id: i64,
    products: &[Product],
    snapshot: &dyn SnapshotSource,
) -> Result<ReconRow> {
    let mut timing = Timing::default();

    let t = Instant::now();
    let (slice, timeline) = timeline::load_user(conn, backend_account_id)?;
    timing.load_us = t.elapsed().as_micros();

    // Always ask the snapshot source — `existed_at_start` is the export's own
    // classification and isn't trusted as ground truth on its own; a live
    // archival lookup is the authoritative check for every account, fresh or
    // not. `Ok(None)` from an authoritative source (archival) means the
    // account genuinely had no state at H, which is a complete answer, not a
    // gap: the first `Deposit` in its timeline creates it, exactly as the real
    // contract's `get_or_create_account_mut` does.
    let t = Instant::now();
    let baseline_raw: std::thread::Result<Result<Option<Vec<u8>>>> =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            snapshot.raw_account(backend_account_id, &slice.near_account_id)
        }));
    timing.fetch_us = t.elapsed().as_micros();

    // A failed/panicked snapshot fetch is an infra problem (network, RPC
    // throttling, a malformed response) — the account itself was never
    // reached, so this is a `failure:`, not an `error:`.
    let raw_account = match baseline_raw {
        Err(_) => {
            timing.log(backend_account_id, "failure:snapshot panic");
            return Ok(zero_calc_row(&slice, "failure:snapshot panic"));
        }
        Ok(Err(e)) => {
            let status = format!("failure:{}", truncate(&e.to_string()));
            timing.log(backend_account_id, &status);
            return Ok(zero_calc_row(&slice, status));
        }
        Ok(Ok(v)) => v,
    };

    let (raw_account, timeline, claimed_adjustment) =
        apply_block_height_fallback(snapshot, &slice, raw_account, timeline);
    // The dropped leading claim (if any) already happened before the
    // recovered baseline — it's out of scope for this replay on both sides.
    let actual = slice.onchain_claimed - claimed_adjustment;

    // A non-authoritative source (the local `snapshots` cache) missing a row
    // is only a real gap for an account the export says existed at H — a
    // fresh account is correctly rowless there regardless.
    let no_baseline = raw_account.is_none() && !snapshot.is_authoritative() && slice.existed_at_start;

    // A malformed near_account_id means the account was never even attempted
    // — same category as a fetch failure, not a modeled contract panic.
    let account_id: near_sdk::AccountId = match slice.near_account_id.parse() {
        Ok(a) => a,
        Err(_) => return Ok(zero_calc_row(&slice, "failure:invalid near_account_id")),
    };

    // `run_timeline` catches contract panics itself, but a second caught panic on
    // one worker thread has been observed to escape `run_timeline`'s internal
    // `catch_unwind` (near-sdk mock harness state after the upfront
    // `set_timezone`); this guard keeps one bad account from killing the worker.
    // An escape here means run_timeline's own panic-safety broke down for this
    // account — a genuine bug, not a modeled contract panic — so it's a
    // `failure:`, not an `error:`.
    //
    // After an escaped panic the worker's thread-local mock storage is in an
    // unknown state; the NEXT account relies on `run_timeline`'s internal
    // `Context::new` calling `blockchain.take_storage()` to drain it. If that
    // drain ever goes away, this guard turns into a silent-divergence risk.
    let t = Instant::now();
    let unwind_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        engine::run_timeline(
            engine::Baseline { account_id, raw_account, timezone_ms: slice.timezone_ms },
            products,
            parse::H_MS,
            timeline,
        )
    }));
    timing.replay_us = t.elapsed().as_micros();

    let outcome = match unwind_result {
        Ok(o) => o,
        Err(e) => {
            let msg = e
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| e.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "engine panic".to_string());
            let status = format!("failure:{}", truncate(&msg));
            timing.log(backend_account_id, &status);
            return Ok(zero_calc_row(&slice, status));
        }
    };

    let calculated = outcome.total_claimed;
    let delta: i128 = i128::try_from(calculated).context("calculated_total_claim exceeds i128")?
        - i128::try_from(actual).context("actual_total_claim exceeds i128")?;
    let rel_delta = if actual == 0 { 0.0 } else { delta as f64 / actual as f64 };

    let status = match &outcome.status {
        ReplayStatus::Error(msg) if no_baseline && msg.contains("is not found") => {
            "no_baseline".to_string()
        }
        ReplayStatus::Error(msg) => format!("error:{}", truncate(msg)),
        ReplayStatus::Ok if no_baseline => "no_baseline".to_string(),
        ReplayStatus::Ok => "ok".to_string(),
    };

    timing.log(backend_account_id, &status);

    Ok(ReconRow {
        account_id: slice.backend_account_id,
        near_account_id: slice.near_account_id,
        calculated_total_claim: calculated.to_string(),
        actual_total_claim: actual.to_string(),
        delta: delta.to_string(),
        rel_delta,
        n_claims: outcome.per_claim.len(),
        status,
    })
}
