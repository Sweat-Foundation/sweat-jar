use sweat_jar_model::{
    data::product::{Apy, Cap, FixedProductTerms, Product, ScoreBasedProductTerms, Terms},
    Timezone,
};
use sweat_jar_primitives::UDecimal;

use super::engine::{run_timeline, Action, Baseline, Event, ReplayStatus, Timeline};

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
                action: Action::Claim,
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
            action: Action::Claim,
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
            Event { ts_ms: 4 * DAY_MS, seq: 3, action: Action::Claim },
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
            Event { ts_ms: 31_536_000_000 + 100, seq: 1, action: Action::Claim },
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
