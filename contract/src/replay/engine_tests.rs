use sweat_jar_model::{
    data::product::{Apy, Cap, FixedProductTerms, Product, ScoreBasedProductTerms, Terms},
    Timezone,
};
use sweat_jar_primitives::UDecimal;

use super::engine::{run_timeline, run_timeline_traced, Action, Baseline, Event, ReplayStatus, Timeline};

const DAY_MS: u64 = 86_400_000;
const HOUR_MS: u64 = 3_600_000;

fn fixed_product() -> Product {
    Product {
        id: "365d_12apy".to_string(),
        cap: Cap::new(1_000_000_000_000_000_000, 500_000 * 10u128.pow(24)),
        terms: Terms::Fixed(FixedProductTerms {
            lockup_term: 31_536_000_000u64.into(),
            apy: Apy::Constant(UDecimal::new(12, 2)),
        }),
        withdrawal_fee: None,
        public_key: None,
        is_enabled: true,
    }
}

fn score_product() -> Product {
    Product {
        id: "steps_365d_20000".to_string(),
        cap: Cap::new(0, 500_000 * 10u128.pow(24)),
        terms: Terms::ScoreBased(ScoreBasedProductTerms {
            score_cap: 20_000,
            lockup_term: 31_536_000_000u64.into(),
        }),
        withdrawal_fee: None,
        public_key: None,
        is_enabled: true,
    }
}

#[test]
fn deposit_then_claim_after_a_year_yields_roughly_apy() {
    let account_id: near_sdk::AccountId = "acc.near".parse().unwrap();
    let one_year_ms = 31_536_000_000u64;
    let deposit_amount = 1_000 * 10u128.pow(18);

    let timeline = Timeline {
        events: vec![
            Event {
                ts_ms: 10,
                seq: 0,
                action: Action::Deposit {
                    product_id: "365d_12apy".into(),
                    amount: deposit_amount,
                },
            },
            Event {
                ts_ms: one_year_ms + 100,
                seq: 1,
                action: Action::Claim { timestamp_ms: one_year_ms + 100 },
            },
        ],
    }
    .sorted();

    let outcome = run_timeline(Baseline { account_id, raw_account: None, timezone_ms: None }, &[fixed_product()], 0, timeline);

    assert!(matches!(outcome.status, ReplayStatus::Ok));
    assert_eq!(outcome.per_claim.len(), 1);
    // 12% of 1000 SWEAT, within 1%.
    let expected = 120 * 10u128.pow(18);
    let claimed = outcome.total_claimed;
    assert!(claimed.abs_diff(expected) < expected / 100, "claimed {claimed} vs {expected}");
}

#[test]
fn claim_with_no_jars_is_reported_not_panicked() {
    let account_id: near_sdk::AccountId = "empty.near".parse().unwrap();
    let timeline = Timeline {
        events: vec![Event {
            ts_ms: 5,
            seq: 0,
            action: Action::Claim { timestamp_ms: 5 },
        }],
    }
    .sorted();
    let outcome = run_timeline(Baseline { account_id, raw_account: None, timezone_ms: None }, &[fixed_product()], 0, timeline);
    // Either Ok with zero claims, or Error — never a process panic.
    match outcome.status {
        ReplayStatus::Ok => assert_eq!(outcome.total_claimed, 0),
        ReplayStatus::Error(_) => {}
    }
}

#[test]
fn score_deposit_gets_its_timezone_before_the_jar_is_created() {
    // A fresh account (no baseline) whose feed carries a timezone: the score jar
    // must be created without the contract's "score based jar without providing
    // time zone" panic. That only holds if `set_timezone` ran *before* the
    // deposit — which is what `set_timezone_before_score_jar` does in the
    // `Deposit` arm.
    let account_id: near_sdk::AccountId = "tz.near".parse().unwrap();
    let steps = 10_000u16;
    let timeline = Timeline {
        events: vec![
            Event {
                ts_ms: DAY_MS,
                seq: 0,
                action: Action::Deposit { product_id: "steps_365d_20000".into(), amount: 1_000 * 10u128.pow(18) },
            },
            Event { ts_ms: 2 * DAY_MS + HOUR_MS, seq: 1, action: Action::RecordScore(vec![(steps, 2 * DAY_MS + HOUR_MS)]) },
            Event { ts_ms: 3 * DAY_MS + HOUR_MS, seq: 2, action: Action::RecordScore(vec![(steps, 3 * DAY_MS + HOUR_MS)]) },
            Event { ts_ms: 4 * DAY_MS, seq: 3, action: Action::Claim { timestamp_ms: 4 * DAY_MS } },
        ],
    }
    .sorted();

    let outcome = run_timeline(
        Baseline { account_id, raw_account: None, timezone_ms: Some(*Timezone::hour_shift(3)) },
        &[score_product()],
        0,
        timeline,
    );
    assert!(matches!(outcome.status, ReplayStatus::Ok), "status: {:?}", outcome.status);
    assert_eq!(outcome.per_claim.len(), 1);
    assert!(outcome.total_claimed > 0, "score jar accrued nothing");
}

#[test]
fn fixed_only_account_with_no_feed_timezone_is_fine() {
    // `set_timezone_before_score_jar` must NOT fire for a non-score deposit: this
    // account's feed timezone is the invalid sentinel and its only jar is Fixed,
    // so no `set_timezone` is attempted and the deposit/claim just work.
    let account_id: near_sdk::AccountId = "fixed.near".parse().unwrap();
    let timeline = Timeline {
        events: vec![
            Event {
                ts_ms: 10,
                seq: 0,
                action: Action::Deposit { product_id: "365d_12apy".into(), amount: 1_000 * 10u128.pow(18) },
            },
            Event { ts_ms: 31_536_000_000 + 100, seq: 1, action: Action::Claim { timestamp_ms: 31_536_000_000 + 100 } },
        ],
    }
    .sorted();

    let outcome = run_timeline(
        Baseline { account_id, raw_account: None, timezone_ms: Some(i64::MIN) },
        &[fixed_product(), score_product()],
        0,
        timeline,
    );
    assert!(matches!(outcome.status, ReplayStatus::Ok), "status: {:?}", outcome.status);
    assert!(outcome.total_claimed > 0);
}

/// A second score-based product, distinct from `score_product()`'s
/// `steps_365d_20000` — the airdrop's target jar, so the account already
/// has an unrelated, pre-existing score-based jar (`score_product()`'s) when
/// the airdrop's `settle_interest_before_booster` runs.
fn score_product_b() -> Product {
    Product {
        id: "steps_365d_20000_b".to_string(),
        cap: Cap::new(0, 500_000 * 10u128.pow(24)),
        terms: Terms::ScoreBased(ScoreBasedProductTerms {
            score_cap: 20_000,
            lockup_term: 31_536_000_000u64.into(),
        }),
        withdrawal_fee: None,
        public_key: None,
        is_enabled: true,
    }
}

/// Decisive check for the `Action::AirdropWithBooster` fix: does replaying
/// `airdrop()`'s real internal order (`settle_interest` BEFORE the new jar
/// exists) actually produce a different claimed total than the naive
/// "separate `Deposit` then `ApplyBooster`" sequence (whose `apply_booster()`
/// call runs `settle_interest` AFTER the new jar already exists)?
///
/// Both variants replay the identical history — an existing score-based jar
/// (`score_product()`) with stale score history, then a same-block
/// deposit-with-booster into a SECOND score-based jar (`score_product_b()`),
/// then a claim — and their totals are compared bit-for-bit.
#[test]
fn airdrop_with_booster_matches_naive_deposit_then_booster_replay() {
    fn run(account_id: &str, second_jar_action_at: u64) -> (u64, near_sdk::AccountId, Vec<Event>) {
        let day = DAY_MS;
        let mut events = vec![
            Event {
                ts_ms: 10,
                seq: 0,
                action: Action::Deposit { product_id: "steps_365d_20000".into(), amount: 1_000 * 10u128.pow(18) },
            },
            Event { ts_ms: day, seq: 1, action: Action::RecordScore(vec![(10_000, day)]) },
        ];
        events.push(match () {
            _ if second_jar_action_at == 0 => unreachable!(),
            _ => Event {
                ts_ms: second_jar_action_at,
                seq: 2,
                action: Action::AirdropWithBooster {
                    product_id: "steps_365d_20000_b".into(),
                    amount: 500 * 10u128.pow(18),
                    score: 5_000,
                    timestamp_ms: second_jar_action_at,
                },
            },
        });
        events.push(Event { ts_ms: 11 * day, seq: 3, action: Action::Claim { timestamp_ms: 11 * day } });
        (day, account_id.parse().unwrap(), events)
    }

    let (_, account_id, events) = run("airdrop.near", 10 * DAY_MS);
    let merged_outcome = run_timeline(
        Baseline { account_id, raw_account: None, timezone_ms: Some(*Timezone::hour_shift(0)) },
        &[score_product(), score_product_b()],
        0,
        Timeline { events }.sorted(),
    );

    // Naive variant: replace the single AirdropWithBooster with a separate
    // Deposit + ApplyBooster pair, same product/amount/score/timestamp.
    let (_, account_id2, mut naive_events) = run("naive.near", 10 * DAY_MS);
    let merged_idx = naive_events
        .iter()
        .position(|e| matches!(e.action, Action::AirdropWithBooster { .. }))
        .unwrap();
    let Action::AirdropWithBooster { product_id, amount, score, timestamp_ms } = naive_events[merged_idx].action.clone()
    else {
        unreachable!()
    };
    let ts = naive_events[merged_idx].ts_ms;
    naive_events.splice(
        merged_idx..=merged_idx,
        [
            Event { ts_ms: ts, seq: 2, action: Action::Deposit { product_id, amount } },
            Event { ts_ms: ts, seq: 3, action: Action::ApplyBooster { score, timestamp_ms } },
        ],
    );
    // Re-number the trailing claim's seq so ordering is still well-formed.
    naive_events.last_mut().unwrap().seq = 4;

    let naive_outcome = run_timeline(
        Baseline { account_id: account_id2, raw_account: None, timezone_ms: Some(*Timezone::hour_shift(0)) },
        &[score_product(), score_product_b()],
        0,
        Timeline { events: naive_events }.sorted(),
    );

    assert!(matches!(merged_outcome.status, ReplayStatus::Ok), "merged status: {:?}", merged_outcome.status);
    assert!(matches!(naive_outcome.status, ReplayStatus::Ok), "naive status: {:?}", naive_outcome.status);
    assert_eq!(
        merged_outcome.total_claimed, naive_outcome.total_claimed,
        "merged={} naive={}",
        merged_outcome.total_claimed, naive_outcome.total_claimed
    );
}

/// Captures the account view right after replaying one `AirdropWithBooster`
/// on a fresh account, to inspect exactly what state it leaves behind.
#[test]
fn airdrop_with_booster_creates_the_jar_and_applies_the_booster() {
    let account_id: near_sdk::AccountId = "single.near".parse().unwrap();
    let timeline = Timeline {
        events: vec![Event {
            ts_ms: DAY_MS,
            seq: 0,
            action: Action::AirdropWithBooster {
                product_id: "steps_365d_20000".into(),
                amount: 1_000 * 10u128.pow(18),
                score: 5_000,
                timestamp_ms: DAY_MS,
            },
        }],
    }
    .sorted();

    let mut views = Vec::new();
    let outcome = run_timeline_traced(
        Baseline { account_id, raw_account: None, timezone_ms: Some(*Timezone::hour_shift(0)) },
        &[score_product()],
        0,
        timeline,
        |_i, view| views.push(view),
    );
    assert!(matches!(outcome.status, ReplayStatus::Ok), "status: {:?}", outcome.status);
    let view = views.last().expect("one captured view");
    let jar = view.jars.get("steps_365d_20000").expect("jar created");
    assert_eq!(jar.deposits[0].1 .0, 1_000 * 10u128.pow(18));
    assert_eq!(view.score.history[0].booster, 5_000);
}

#[test]
fn same_millisecond_deposit_and_booster_replay_in_log_index_order() {
    // Regression for a real production account: a `deposit` (log_index 0) and
    // an `apply_booster` (log_index 1) landed in the exact same millisecond —
    // same receipt, sequential logs. `Timeline::sorted()` used to sort by
    // `(ts_ms, rank, seq)`, and ApplyBooster's rank (0) outranks Deposit's (1),
    // so the booster replayed BEFORE the deposit that sets the account's
    // timezone — a spurious "Timezone is not set" panic on an account that, on
    // chain, never hit that condition. `seq` (real log_index) must win ties
    // over the synthetic `rank`.
    let account_id: near_sdk::AccountId = "tz-race.near".parse().unwrap();
    let timeline = Timeline {
        events: vec![
            Event {
                ts_ms: 1_000,
                seq: 0, // log_index 0: deposit executed first on-chain
                action: Action::Deposit { product_id: "steps_365d_20000".into(), amount: 1_000 * 10u128.pow(18) },
            },
            Event {
                ts_ms: 1_000,
                seq: 1, // log_index 1: booster applied second, same receipt
                action: Action::ApplyBooster { score: 3000, timestamp_ms: 1_000 },
            },
        ],
    }
    .sorted();

    assert_eq!(timeline.events[0].seq, 0, "deposit (seq 0) must sort first");
    assert_eq!(timeline.events[1].seq, 1, "booster (seq 1) must sort second");

    let outcome = run_timeline(
        Baseline { account_id, raw_account: None, timezone_ms: Some(*Timezone::hour_shift(3)) },
        &[score_product()],
        0,
        timeline,
    );
    assert!(matches!(outcome.status, ReplayStatus::Ok), "status: {:?}", outcome.status);
}
