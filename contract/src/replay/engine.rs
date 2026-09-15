//! Reusable timeline-execution engine for account replays.
//!
//! [`run_timeline`] runs an ordered list of [`Event`]s against a fresh
//! in-process contract on the current thread, returning a [`ReplayOutcome`].
//! Contract panics are caught into [`ReplayStatus::Error`].
//!
//! NOTE: a second caught panic on one thread has been observed to escape this
//! guard (near-sdk mock harness state), so callers replaying many accounts on
//! one worker thread MUST wrap this call in their own `catch_unwind` (see
//! `replay::reconcile::reconcile_user`).

use std::panic::{catch_unwind, AssertUnwindSafe};

use near_sdk::{
    json_types::{Base64VecU8, I64},
    AccountId, PromiseOrValue,
};
pub use sweat_jar_model::data::product::Product;
use sweat_jar_model::{
    api::{AccountApi, ClaimApi, RestakeApi, WithdrawApi},
    data::{account::features::Feature, deposit::DepositTicket},
    Timezone,
};
pub use sweat_jar_model::{Score, UTC};

use crate::{
    common::{env::test_env_ext, testing::Context},
    migration::api::store_account_raw,
};

/// A single account interaction to replay.
#[derive(Clone, Debug)]
pub enum Action {
    /// One `record_score` call: `(score, timestamp_ms)` increments, exactly as
    /// the oracle batched them for this step package — a regular package sends
    /// `[(steps, created_at), (yesterday_steps, created_at − 24h)]`.
    RecordScore(Vec<(Score, u64)>),
    Deposit { product_id: String, amount: u128 },
    Withdraw { product_id: String },
    /// `restake(from, ticket(into), None, Some(amount))` — a single-source
    /// restake, which may target a different product (`from != into`).
    /// `amount` is the principal restaked on-chain (`jar_events.amount`); the
    /// rest of the matured principal is withdrawn.
    Restake { from: String, into: String, amount: u128 },
    SetIncreasedScoreCap(bool),
    /// `claim_total()` — `timestamp_ms` is the mock block time to run it at.
    /// `claim_total` uses `env::block_timestamp_ms()` as `now` for every
    /// jar's interest calculation AND embeds that exact same value into the
    /// emitted `claim` event's own `timestamp` field — so that field is a
    /// precise, ground-truth record of what the real contract used, more
    /// accurate than the export's block-level `block_timestamp_utc` (which
    /// can lag the real per-receipt execution instant by ~1s on average,
    /// occasionally by tens of seconds — the same class of export artifact
    /// already documented for `record_score`/`apply_booster`, but for claims
    /// this previously went uncorrected because the payload's timestamp
    /// field was parsed and then discarded). See
    /// `replay/README.md`'s "Known error causes".
    Claim { timestamp_ms: u64 },
    /// `apply_booster([account], score, UTC(timestamp_ms))` — the oracle booster path.
    ApplyBooster { score: Score, timestamp_ms: u64 },
    /// `withdraw_all(Some(product_ids))` — matured balance of the named jars.
    WithdrawAll { product_ids: Vec<String> },
    /// `restake_all(ticket(into=product_id), None, Some(amount))`.
    RestakeAll { product_id: String, amount: u128 },
    /// A single `airdrop()` receiver step, replayed with the SAME internal
    /// order the real contract uses (`settle_interest_before_booster` ->
    /// `create_airdrop_deposit` -> `apply_airdrop_booster`) — critically,
    /// `settle_interest` runs BEFORE the jar is created here. A standalone
    /// `Deposit` followed by a standalone `ApplyBooster` (whose own
    /// `apply_booster()` call internally re-triggers `settle_interest`) would
    /// instead settle interest AFTER the jar already exists, wrongly
    /// re-caching a jar that didn't exist yet on the real chain at that
    /// point. The caller (`replay/src/timeline.rs`) detects this pattern as
    /// a `deposit` and `apply_booster` event sharing one on-chain block.
    AirdropWithBooster { product_id: String, amount: u128, score: Score, timestamp_ms: u64 },
}

impl Action {
    /// Fallback tie-break for two events sharing BOTH `ts_ms` and `seq`
    /// (`seq` is the real on-chain `log_index`; a tie there means two
    /// different receipts landed in the same millisecond with the same
    /// intra-receipt log position, so log_index alone can't order them) —
    /// scores land first, then state-changing calls, then claims (so a claim
    /// sees up-to-date state). Genuinely ambiguous in that case; `seq` decides
    /// everything else, see [`Timeline::sorted`].
    pub fn rank(&self) -> u8 {
        match self {
            Action::RecordScore(_) | Action::ApplyBooster { .. } => 0,
            Action::Deposit { .. }
            | Action::Withdraw { .. }
            | Action::Restake { .. }
            | Action::SetIncreasedScoreCap(_)
            | Action::WithdrawAll { .. }
            | Action::RestakeAll { .. }
            | Action::AirdropWithBooster { .. } => 1,
            Action::Claim { .. } => 2,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Event {
    pub ts_ms: u64,
    pub seq: u64,
    pub action: Action,
}

#[derive(Default)]
pub struct Timeline {
    pub events: Vec<Event>,
}

impl Timeline {
    /// Sorts events by `(ts_ms, seq, rank)` in place. `seq` is the real
    /// on-chain `log_index` and is authoritative intra-block order — it MUST
    /// outrank the synthetic `rank` heuristic. Sorting by `(ts_ms, rank, seq)`
    /// instead (the pre-event-sourced-pivot order, when `seq` was only a
    /// synthetic ingest counter) silently reordered same-millisecond events
    /// against their real on-chain order — e.g. a `deposit` that sets an
    /// account's timezone (log_index 0) landing AFTER an `apply_booster` in
    /// the same receipt (log_index 1) purely because `ApplyBooster` outranks
    /// `Deposit`, causing a spurious "Timezone is not set" panic on replay.
    pub fn sorted(mut self) -> Self {
        self.events.sort_by_key(|e| (e.ts_ms, e.seq, e.action.rank()));
        self
    }
}

/// Opaque baseline: the raw borsh bytes of an `AccountVersioned`, or `None` for
/// a fresh account.
pub struct Baseline {
    pub account_id: AccountId,
    pub raw_account: Option<Vec<u8>>,
    /// Authoritative account timezone (ms offset). `None` or `i64::MIN` -> not set.
    pub timezone_ms: Option<i64>,
}

#[derive(Debug)]
pub enum ReplayStatus {
    Ok,
    Error(String),
}

#[derive(Debug)]
pub struct ReplayOutcome {
    pub total_claimed: u128,
    pub per_claim: Vec<(u64, u128)>,
    pub status: ReplayStatus,
}

/// Runs the timeline against a fresh in-process contract on the current thread.
///
/// Resets thread-local mock storage before starting: the fresh [`Context`] the
/// closure builds calls `blockchain.take_storage()`, so nothing carries over
/// from a previous account on this thread.
///
/// Contract panics are caught into [`ReplayStatus::Error`], but see the module
/// doc: a second caught panic on one thread can still escape, so a caller
/// looping over many accounts must add its own `catch_unwind`.
///
/// Note: `Action::Withdraw` withdraws the entire liquid principal of the
/// product's jar — the contract has no partial-amount withdraw — so historical
/// partial withdrawals are a known divergence.
pub fn run_timeline(baseline: Baseline, products: &[Product], window_start_ms: u64, timeline: Timeline) -> ReplayOutcome {
    run_timeline_traced(baseline, products, window_start_ms, timeline, |_, _| {})
}

/// Same as [`run_timeline`], but calls `on_step(index, account_view)` after
/// every processed event — `index` is 0-based into the (already sorted)
/// `timeline.events`, `account_view` the account's full state right after
/// that event executed. For bisecting a divergence against real archival
/// state: capture the view at each event, then compare each one to
/// `get_account` at that event's own on-chain block height (NEAR's
/// view-at-height semantics mean that block already reflects the event) to
/// find exactly where the two histories first disagree.
///
/// `run_timeline` is just this with a no-op callback — the callback costs
/// nothing on the hot path (no allocation, no view construction) when it
/// does nothing with its argument, so there's no reason to keep two
/// separate implementations of the event loop in sync.
pub fn run_timeline_traced(
    baseline: Baseline,
    products: &[Product],
    window_start_ms: u64,
    timeline: Timeline,
    mut on_step: impl FnMut(usize, sweat_jar_model::data::account::view::AccountView),
) -> ReplayOutcome {
    test_env_ext::set_test_log_events(false);

    let account_id = baseline.account_id.clone();
    let result = catch_unwind(AssertUnwindSafe(|| {
        let mut context = Context::new(admin()).with_products(products);

        if let Some(raw) = &baseline.raw_account {
            store_account_raw(account_id.clone(), Base64VecU8(raw.clone()));
        }
        context.set_block_timestamp_in_ms(window_start_ms);

        let mut total_claimed = 0u128;
        let mut per_claim: Vec<(u64, u128)> = Vec::new();
        // The timezone is set (Oracle) right before the account's first
        // score-based jar is created — mirroring the oracle setting it ahead of
        // the first score deposit. `try_set_timezone` on the contract is a no-op
        // when the baseline already carried a valid timezone, so this never
        // overrides on-chain state.
        let mut timezone_applied = false;

        for (index, event) in timeline.events.into_iter().enumerate() {
            context.set_block_timestamp_in_ms(event.ts_ms);
            match event.action {
                Action::RecordScore(increments) => {
                    context.switch_account_to_operator();
                    let increments: Vec<(Score, UTC)> =
                        increments.into_iter().map(|(s, ts)| (s, UTC(ts))).collect();
                    context
                        .contract()
                        .record_score(vec![(account_id.clone(), increments)]);
                }
                Action::Deposit { product_id, amount } => {
                    set_timezone_before_score_jar(
                        &mut context,
                        &account_id,
                        baseline.timezone_ms,
                        products,
                        &product_id,
                        &mut timezone_applied,
                    );
                    let ticket = DepositTicket {
                        product_id,
                        valid_until: 0.into(),
                        // Fallback for accounts the feed has no timezone for; the
                        // contract keeps an already-set timezone and ignores this.
                        timezone: Some(Timezone::hour_shift(0)),
                    };
                    context.switch_account_to_ft_contract_account();
                    context.contract().deposit(account_id.clone(), ticket, amount, None);
                }
                Action::Withdraw { product_id } => {
                    context.switch_account(&account_id);
                    let _ = context.contract().withdraw(product_id);
                }
                Action::Restake { from, into, amount } => {
                    set_timezone_before_score_jar(
                        &mut context,
                        &account_id,
                        baseline.timezone_ms,
                        products,
                        &into,
                        &mut timezone_applied,
                    );
                    context.switch_account(&account_id);
                    let ticket = DepositTicket {
                        product_id: into.clone(),
                        valid_until: 0.into(),
                        timezone: Some(Timezone::hour_shift(0)),
                    };
                    // Restake exactly what was restaked on-chain; the rest of the
                    // matured principal is withdrawn (matching the contract).
                    let _ = context.contract().restake(from, ticket, None, Some(amount.into()));
                }
                Action::SetIncreasedScoreCap(enabled) => {
                    context.switch_account_to_operator();
                    context
                        .contract()
                        .set_feature_enabled(account_id.clone(), Feature::IncreasedScoreCap, enabled);
                }
                Action::Claim { timestamp_ms } => {
                    context.switch_account(&account_id);
                    // Use the claim's own embedded execution timestamp
                    // (ground truth — it's exactly what the real
                    // `claim_total()` call used for `now`, and what it
                    // embedded into the emitted event) instead of the outer
                    // block-level `ts_ms`, which can lag the real per-receipt
                    // execution instant.
                    context.set_block_timestamp_in_ms(timestamp_ms);
                    if let PromiseOrValue::Value(claimed) = context.contract().claim_total(None) {
                        let amount = claimed.get_total().0;
                        total_claimed += amount;
                        // Reported/keyed by the event's own `ts_ms` (matching
                        // how the caller looks up the on-chain claim total for
                        // this same event), not `timestamp_ms`.
                        per_claim.push((event.ts_ms, amount));
                    }
                }
                Action::ApplyBooster { score, timestamp_ms } => {
                    context.switch_account_to_operator();
                    context
                        .contract()
                        .apply_booster(vec![account_id.clone()], score, UTC(timestamp_ms));
                }
                Action::WithdrawAll { product_ids } => {
                    context.switch_account(&account_id);
                    let set: std::collections::HashSet<String> = product_ids.into_iter().collect();
                    let _ = context.contract().withdraw_all(Some(set));
                }
                Action::RestakeAll { product_id, amount } => {
                    set_timezone_before_score_jar(
                        &mut context,
                        &account_id,
                        baseline.timezone_ms,
                        products,
                        &product_id,
                        &mut timezone_applied,
                    );
                    context.switch_account(&account_id);
                    let ticket = DepositTicket {
                        product_id: product_id.clone(),
                        valid_until: 0.into(),
                        timezone: Some(Timezone::hour_shift(0)),
                    };
                    let _ = context.contract().restake_all(ticket, None, Some(amount.into()));
                }
                Action::AirdropWithBooster { product_id, amount, score, timestamp_ms } => {
                    set_timezone_before_score_jar(
                        &mut context,
                        &account_id,
                        baseline.timezone_ms,
                        products,
                        &product_id,
                        &mut timezone_applied,
                    );
                    context.switch_account_to_operator();
                    // Mirrors `airdrop()`'s exact per-receiver order — see the
                    // `Action::AirdropWithBooster` doc comment. `settle_interest`
                    // MUST run before the jar exists (`settle_interest_before_booster`),
                    // then the deposit creates it, then the booster is applied
                    // WITHOUT going through the public `apply_booster()` API
                    // (which would re-run `settle_interest` a second time, now
                    // seeing the jar that just got created — the exact bug this
                    // action exists to avoid).
                    let ticket = DepositTicket {
                        product_id: product_id.clone(),
                        valid_until: 0.into(),
                        timezone: Some(Timezone::hour_shift(0)),
                    };
                    context.contract().settle_interest_before_booster(&account_id, score);
                    let product = context.contract().get_product(&product_id);
                    context.contract().create_airdrop_deposit(&account_id, &ticket, amount, &product, event.ts_ms);
                    let (mut applied, mut rejected) = (Vec::new(), Vec::new());
                    context.contract().apply_airdrop_booster(
                        &account_id,
                        score,
                        Some(UTC(timestamp_ms)),
                        &mut applied,
                        &mut rejected,
                    );
                }
            }
            if let Some(view) = AccountApi::get_account(&*context.contract(), account_id.clone()) {
                on_step(index, view);
            }
        }
        (total_claimed, per_claim)
    }));

    match result {
        Ok((total_claimed, per_claim)) => ReplayOutcome {
            total_claimed,
            per_claim,
            status: ReplayStatus::Ok,
        },
        Err(e) => {
            let raw = e
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| e.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            ReplayOutcome {
                total_claimed: 0,
                per_claim: Vec::new(),
                status: ReplayStatus::Error(unwrap_guest_panic(&raw)),
            }
        }
    }
}

fn admin() -> AccountId {
    "admin.near".parse().unwrap()
}

/// Set the account's timezone (Oracle) immediately before its first score-based
/// jar is created, if the feed supplied one and it has not been set yet this
/// run. `product_id` is the product the jar is being created in; the call is a
/// no-op unless that product is score-based. The contract's `try_set_timezone`
/// additionally no-ops when a baseline already carried a valid timezone.
fn set_timezone_before_score_jar(
    context: &mut Context,
    account_id: &AccountId,
    timezone_ms: Option<i64>,
    products: &[Product],
    product_id: &str,
    applied: &mut bool,
) {
    if *applied {
        return;
    }
    let Some(tz) = timezone_ms.filter(|t| *t != i64::MIN) else {
        return;
    };
    let is_score_based = products
        .iter()
        .find(|p| p.id == product_id)
        .is_some_and(|p| p.terms.is_score_based());
    if !is_score_based {
        return;
    }
    context.switch_account_to_operator();
    context.contract().set_timezone(account_id.clone(), I64(tz));
    *applied = true;
}

/// near-sdk's mock wraps a guest `panic_str` as
/// `called \`Result::unwrap()\` on an \`Err\` value: HostError(GuestPanic { panic_msg: "…" })`.
/// Pull the inner `panic_msg` out so `error:` rows carry the real reason.
fn unwrap_guest_panic(raw: &str) -> String {
    if let Some(start) = raw.find("panic_msg: \"") {
        let rest = &raw[start + "panic_msg: \"".len()..];
        if let Some(end) = rest.rfind("\" }") {
            return rest[..end].to_string();
        }
    }
    raw.to_string()
}

/// Parses an `account_state` JSON object (the shape of
/// `test_data/account_full_state_190375496.json`'s `account_state` field) into an `Account`.
/// `score.updated_at` of 0 (or missing) is stamped to `window_start_ms` to avoid the
/// `AccountScore::default()` current-block-time hazard.
pub fn parse_account_state(
    state: &near_sdk::serde_json::Value,
    window_start_ms: u64,
) -> sweat_jar_model::data::account::Account {
    use std::collections::HashMap;

    use sweat_jar_model::{
        data::{
            account::{
                features::{Feature, Features},
                Account,
            },
            jar::{Deposit, Jar, JarCache},
        },
        AccountScore, DailyScore, Score, UTC,
    };

    let mut jars: HashMap<String, Jar> = HashMap::new();
    for (product_id, jar_json) in state["jars"].as_object().expect("jars object") {
        let deposits = jar_json["deposits"]
            .as_array()
            .expect("deposits array")
            .iter()
            .map(|pair| {
                let created_at: u64 = pair[0].as_str().unwrap().parse().unwrap();
                let principal: u128 = pair[1].as_str().unwrap().parse().unwrap();
                Deposit::new(created_at, principal)
            })
            .collect();

        let cache = jar_json.get("cache").filter(|c| !c.is_null()).map(|c| JarCache {
            updated_at: c["updated_at"].as_str().unwrap().parse().unwrap(),
            interest: c["interest"].as_str().unwrap().parse().unwrap(),
        });

        jars.insert(
            product_id.clone(),
            Jar {
                deposits,
                cache,
                is_locked: jar_json["is_pending_withdraw"].as_bool().unwrap_or(false),
                claim_remainder: jar_json["claim_remainder"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
            },
        );
    }

    let score_json = &state["score"];
    let history = score_json["history"].as_array().expect("score history");
    let daily = |i: usize| -> DailyScore {
        history.get(i).map_or_else(DailyScore::default, |d| DailyScore {
            value: d["value"].as_u64().unwrap_or(0) as Score,
            booster: d["booster"].as_u64().unwrap_or(0) as Score,
        })
    };
    // `updated_at` of 0 or missing -> window start, dodging the AccountScore::default() block-time hazard.
    let updated_at = match score_json["updated_at"].as_u64() {
        Some(0) | None => window_start_ms,
        Some(v) => v,
    };
    let score = AccountScore::new(UTC(updated_at), [daily(0), daily(1)]);

    let mut features = Features::new();
    let f = &state["features"];
    features.set_feature_enabled(&Feature::IncreasedApy, f["increased_apy"].as_bool().unwrap_or(false));
    features.set_feature_enabled(
        &Feature::IncreasedScoreCap,
        f["increased_score_cap"].as_bool().unwrap_or(false),
    );

    Account {
        nonce: state["nonce"].as_u64().unwrap_or(0) as u32,
        jars,
        // `AccountView.timezone` is the raw ms shift from UTC (or `i64::MIN` when
        // the account never set one) — stored verbatim, no hours→ms conversion.
        timezone: Timezone::new(state["timezone"].as_i64().unwrap_or(i64::MIN)),
        score,
        features,
    }
}

#[cfg(test)]
mod action_tests {
    use super::*;

    #[test]
    fn new_action_variants_construct() {
        let _ = Action::ApplyBooster { score: 3000, timestamp_ms: 1 };
        let _ = Action::WithdrawAll { product_ids: vec!["p".into()] };
        let _ = Action::RestakeAll { product_id: "p".into(), amount: 1 };
        assert_eq!(Action::ApplyBooster { score: 0, timestamp_ms: 0 }.rank(), 0);
        assert_eq!(Action::WithdrawAll { product_ids: vec![] }.rank(), 1);
        assert_eq!(
            Action::Restake { from: "a".into(), into: "b".into(), amount: 1 }.rank(),
            1
        );
    }
}
