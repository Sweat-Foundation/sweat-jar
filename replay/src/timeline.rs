//! Per-account event slice -> engine [`Timeline`] synthesis.

use anyhow::{Context, Result};
use duckdb::Connection;
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
            ParsedEvent::Claim { total } => {
                onchain_claimed =
                    onchain_claimed.checked_add(total).context("onchain_claimed overflow")?;
                claim_amounts.insert((ts_ms, log_index), total);
                Action::Claim
            }
        };
        events.push(Event { ts_ms, seq: log_index, action });
        block_heights.insert((ts_ms, log_index), block_height);
    }

    let timeline = Timeline { events }.sorted();
    let first_key = timeline.events.first().map(|first| (first.ts_ms, first.seq));
    let first_event_block_height = first_key.and_then(|k| block_heights.get(&k).copied());
    let first_event_claim_amount = first_key.and_then(|k| claim_amounts.get(&k).copied());

    Ok((
        UserSlice {
            backend_account_id,
            near_account_id,
            existed_at_start,
            timezone_ms,
            onchain_claimed,
            first_event_block_height,
            first_event_claim_amount,
        },
        timeline,
    ))
}
