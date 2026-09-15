//! Per-account event slice -> engine [`Timeline`] synthesis.

use anyhow::{Context, Result};
use duckdb::{Connection, OptionalExt};
use sweat_jar::replay::engine::{Action, Event, Timeline};

use crate::payload::{parse_event, ParsedEvent};

/// One account's on-chain summary alongside its synthesized timeline.
pub struct UserSlice {
    pub backend_account_id: i64,
    pub near_account_id: String,
    pub existed_at_start: bool,
    pub timezone_ms: Option<i64>,
    /// Sum of every `claim` event's payload `items` in the replay window.
    pub onchain_claimed: u128,
    /// Block height of the timeline's first event (post-sort), if it has any
    /// events. Used by the block-height-fallback: when the block-H baseline
    /// is confirmed empty but this first action isn't a `Deposit`, the
    /// account must have been created invisibly (e.g. an FT-transfer
    /// migration) before this block — so re-fetching state at this height
    /// recovers it.
    pub first_event_block_height: Option<u64>,
    /// The on-chain claim amount of the first event, if it's a `Claim`. When
    /// the block-height-fallback drops that first event (its effect is
    /// already reflected in the re-fetched baseline), this amount must also
    /// come out of `onchain_claimed` — otherwise the claim counts on the
    /// on-chain side but never on the calculated side, a spurious ~100%
    /// divergence rather than a fixed reconciliation.
    pub first_event_claim_amount: Option<u128>,
    /// This account's `migrations` table row, if the export's
    /// `jars_merge_events` recorded a JarsMerge on the old (pre-v2) contract
    /// for it — the authoritative "this account was invisibly created by an
    /// `FtMessage::Migrate` within the window" signal, replacing guesswork
    /// with a direct lookup wherever it's available.
    pub migration: Option<MigrationRecord>,
}

/// One row of the `migrations` table — see `db::schema` for how it's built.
pub struct MigrationRecord {
    /// Block height of the JarsMerge event on the old contract — always
    /// strictly after v2's own migrated-state write landed (it's that
    /// write's cross-contract-call callback), so state fetched here is
    /// never mid-flight/locked.
    pub block_height: u64,
    /// The exact borsh bytes v2's `store_account_raw` wrote, when the export
    /// captured them (~98% of rows) — use directly, no RPC needed. `None`
    /// means the export didn't capture them for this account; fetch state at
    /// `block_height` instead (a single try — no lock/walk-back needed).
    pub raw_account: Option<Vec<u8>>,
}

/// Merges an adjacent `Deposit` + `ApplyBooster` pair sharing the same
/// on-chain block into a single `Action::AirdropWithBooster`.
///
/// The `airdrop()` contract call handles a receiver in one internal sequence
/// — `settle_interest_before_booster` (runs BEFORE the jar exists) ->
/// `create_airdrop_deposit` -> `apply_airdrop_booster` — then emits a
/// `Deposit` event per receiver and one batched `ApplyBooster` event after
/// the whole receiver loop. Replaying these as two independent top-level API
/// calls (`Action::Deposit` then `Action::ApplyBooster`) is wrong: the
/// public `apply_booster()` API re-runs `settle_interest` itself, and by
/// then the jar the standalone `Deposit` just created already exists — so it
/// gets wrongly re-cached to `start_of_the_day`, something that never
/// happened on the real chain (confirmed against a real account's archival
/// state and the `interest_replay_receipt_order_cohort` export's
/// `is_deposit_with_booster` flag, which is ~1:1 with this exact
/// same-block-Deposit-and-ApplyBooster pattern in the tracked events).
///
/// Detected purely from this account's own events — no extra data source
/// needed: a `Deposit` and an `ApplyBooster` sharing one on-chain block ID.
/// `block_heights` is keyed by the pre-merge `(ts_ms, seq)`, so the merged
/// event keeps one of the two original `seq` values (the smaller) as its
/// identity — `first_event_block_height`'s lookup (by that same key) still
/// resolves correctly if this merged event turns out to be the timeline's
/// first.
fn merge_airdrop_boosters(events: Vec<Event>, block_heights: &std::collections::HashMap<(u64, u64), u64>) -> Vec<Event> {
    let mut merged = Vec::with_capacity(events.len());
    let mut i = 0;
    while i < events.len() {
        if i + 1 < events.len() {
            let a = &events[i];
            let b = &events[i + 1];
            let same_block = a.ts_ms == b.ts_ms
                && block_heights.get(&(a.ts_ms, a.seq)) == block_heights.get(&(b.ts_ms, b.seq));
            let combo = if same_block {
                match (&a.action, &b.action) {
                    (Action::Deposit { product_id, amount }, Action::ApplyBooster { score, timestamp_ms })
                    | (Action::ApplyBooster { score, timestamp_ms }, Action::Deposit { product_id, amount }) => {
                        Some((product_id.clone(), *amount, *score, *timestamp_ms))
                    }
                    _ => None,
                }
            } else {
                None
            };
            if let Some((product_id, amount, score, timestamp_ms)) = combo {
                merged.push(Event {
                    ts_ms: a.ts_ms,
                    seq: a.seq.min(b.seq),
                    action: Action::AirdropWithBooster { product_id, amount, score, timestamp_ms },
                });
                i += 2;
                continue;
            }
        }
        merged.push(events[i].clone());
        i += 1;
    }
    merged
}

/// Reads every event row for `backend_account_id` and builds a sorted engine
/// [`Timeline`]. An account with no replayable events yields an empty timeline;
/// an account absent from the `accounts` table is an error.
pub fn load_user(conn: &Connection, backend_account_id: i64) -> Result<(UserSlice, Timeline)> {
    let (near_account_id, existed_at_start, timezone_ms): (String, bool, Option<i64>) = conn
        .query_row(
            "SELECT near_account_id, existed_at_start, timezone_ms FROM accounts \
             WHERE backend_account_id = ?",
            duckdb::params![backend_account_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .with_context(|| format!("account {backend_account_id} not in accounts table"))?;

    let mut stmt = conn.prepare(
        "SELECT ts_ms, log_index, event, role, payload, block_height FROM events \
         WHERE backend_account_id = ? ORDER BY ts_ms, log_index",
    )?;
    let rows = stmt
        .query_map(duckdb::params![backend_account_id], |r| {
            Ok((
                r.get::<_, i64>(0)? as u64,
                r.get::<_, i64>(1)? as u64,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)? as u64,
            ))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;

    let mut events = Vec::new();
    // Keyed by (ts_ms, seq) — unique per account, and exactly the sort
    // prefix `Timeline::sorted()` uses — so the first sorted event's block
    // height can be looked back up after sorting.
    let mut block_heights: std::collections::HashMap<(u64, u64), u64> = std::collections::HashMap::new();
    // Same key, for `first_event_claim_amount` — only claim events have an entry.
    let mut claim_amounts: std::collections::HashMap<(u64, u64), u128> = std::collections::HashMap::new();
    let mut onchain_claimed = 0u128;

    for (ts_ms, log_index, event, role, payload, block_height) in rows {
        let Some(parsed) = parse_event(&event, role.as_deref(), &payload)
            .with_context(|| format!("account {backend_account_id} ts {ts_ms} event {event}"))?
        else {
            continue;
        };
        let action = match parsed {
            // `record_score`'s emitted event stores each pair's *Local*
            // (timezone-adjusted) timestamp, not the raw UTC the oracle
            // submitted (`ScoreData.score: Vec<(Score, Local)>`,
            // `contract/src/common/event.rs`). The engine's `record_score`
            // call re-applies the account's timezone shift on the way in, so
            // feeding the Local value straight back in double-applies it —
            // for a nonzero-timezone account, a step landing within
            // `|timezone|` of local midnight gets bucketed into the wrong
            // calendar day (verified against the real contract: see the
            // investigation in `replay/README.md`'s "Known error causes").
            // Subtract the timezone to recover the raw UTC value first.
            //
            // Only `SUCCESS_VALUE` rows are ingested, so the on-chain call
            // already passed `assert_not_future` against this very block
            // time: a recovered UTC timestamp after `ts_ms` is an export
            // artifact (the payload disagreeing with its own
            // `block_timestamp_utc`), never a real state — clamped the same
            // way as `ApplyBooster` below.
            ParsedEvent::RecordScore(pairs) => {
                let tz = timezone_ms.unwrap_or(0);
                Action::RecordScore(
                    pairs
                        .into_iter()
                        .map(|(score, local_ts)| {
                            let utc_ts = (local_ts as i64 - tz).max(0) as u64;
                            (score, utc_ts.min(ts_ms))
                        })
                        .collect(),
                )
            }
            ParsedEvent::ApplyBooster { score, timestamp_ms } => {
                Action::ApplyBooster { score, timestamp_ms: timestamp_ms.min(ts_ms) }
            }
            ParsedEvent::Deposit { product_id, amount } => Action::Deposit { product_id, amount },
            ParsedEvent::WithdrawAll { product_ids } => Action::WithdrawAll { product_ids },
            // `from` is the set of jars actually consumed. One source -> the
            // single-jar `restake(from, into)` call, which may cross products;
            // more than one -> the `restake_all` sweep.
            ParsedEvent::Restake { into, from, restaked } => {
                let mut from = from.into_iter();
                match (from.next(), from.next()) {
                    (Some(single), None) => {
                        Action::Restake { from: single, into, amount: restaked }
                    }
                    _ => Action::RestakeAll { product_id: into, amount: restaked },
                }
            }
            ParsedEvent::SetIncreasedScoreCap(v) => Action::SetIncreasedScoreCap(v),
            ParsedEvent::Claim { total, timestamp_ms } => {
                onchain_claimed =
                    onchain_claimed.checked_add(total).context("onchain_claimed overflow")?;
                claim_amounts.insert((ts_ms, log_index), total);
                Action::Claim { timestamp_ms }
            }
        };
        events.push(Event { ts_ms, seq: log_index, action });
        block_heights.insert((ts_ms, log_index), block_height);
    }

    let timeline = Timeline { events }.sorted();
    let timeline = Timeline { events: merge_airdrop_boosters(timeline.events, &block_heights) };
    let first_key = timeline.events.first().map(|first| (first.ts_ms, first.seq));
    let first_event_block_height = first_key.and_then(|k| block_heights.get(&k).copied());
    let first_event_claim_amount = first_key.and_then(|k| claim_amounts.get(&k).copied());

    let migration = conn
        .query_row(
            "SELECT block_height, raw_account FROM migrations WHERE backend_account_id = ?",
            duckdb::params![backend_account_id],
            |r| {
                Ok(MigrationRecord {
                    block_height: r.get::<_, i64>(0)? as u64,
                    raw_account: r.get::<_, Option<Vec<u8>>>(1)?,
                })
            },
        )
        .optional()
        .context("query migrations table")?;

    Ok((
        UserSlice {
            backend_account_id,
            near_account_id,
            existed_at_start,
            timezone_ms,
            onchain_claimed,
            first_event_block_height,
            first_event_claim_amount,
            migration,
        },
        timeline,
    ))
}
