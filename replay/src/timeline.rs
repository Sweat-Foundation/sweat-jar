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
        "SELECT ts_ms, log_index, event, role, payload FROM events \
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
            ))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;

    let mut events = Vec::new();
    let mut onchain_claimed = 0u128;

    for (ts_ms, log_index, event, role, payload) in rows {
        let Some(parsed) = parse_event(&event, role.as_deref(), &payload)
            .with_context(|| format!("account {backend_account_id} ts {ts_ms} event {event}"))?
        else {
            continue;
        };
        let action = match parsed {
            // Only `SUCCESS_VALUE` rows are ingested, so the on-chain call already
            // passed `assert_not_future` against this very block time: an increment
            // timestamp after `ts_ms` is an export artifact (the payload ts
            // disagreeing with its own `block_timestamp_utc`), never a real state.
            // Clamping keeps replay from panicking on it; `assert_not_future`
            // adjusts both sides by the timezone, so this is timezone-independent.
            ParsedEvent::RecordScore(pairs) => Action::RecordScore(
                pairs.into_iter().map(|(score, ts)| (score, ts.min(ts_ms))).collect(),
            ),
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
                Action::Claim
            }
        };
        events.push(Event { ts_ms, seq: log_index, action });
    }

    Ok((
        UserSlice { backend_account_id, near_account_id, existed_at_start, timezone_ms, onchain_claimed },
        Timeline { events }.sorted(),
    ))
}
