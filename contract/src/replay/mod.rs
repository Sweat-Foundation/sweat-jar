//! Replays a real mainnet account against the local contract.
//!
//! Given a snapshot of an account's on-chain state at some block and the list of
//! interactions (deposits / claims / feature toggles) and step-score recordings
//! that happened afterwards, this test re-runs the whole timeline locally and
//! prints every claim as `timestamp - amount` plus the running total.
//!
//! The replay window is `[snapshot block time, JAR_REPLAY_END]`; events outside
//! it are ignored.
//!
//! Inputs are resolved from env vars, falling back to the files under
//! `test_data/`:
//!
//! | env var                    | default                                                        |
//! |----------------------------|----------------------------------------------------------------|
//! | `JAR_REPLAY_STATE`         | `test_data/account_full_state_190375496.json`                   |
//! | `JAR_REPLAY_INTERACTIONS`  | `test_data/jar_interactions.csv`                                |
//! | `JAR_REPLAY_STEPS`         | `test_data/steps.csv`                                           |
//! | `JAR_REPLAY_END`           | `1788174657961` (2026-08-31T11:10:57.961Z) — window end, ms     |
//!
//! Run with output visible:
//! `cargo test -p sweat_jar replay -- --nocapture`
#![cfg(any(test, feature = "replay-engine"))]

pub mod engine;

#[cfg(test)]
mod engine_tests;

#[cfg(test)]
mod scenario {
    use std::fs;

    use near_sdk::{borsh::to_vec, serde_json, serde_json::Value, AccountId};
    use sweat_jar_model::{
        data::{
            account::{versioned::AccountVersioned, Account},
            product::{
                Apy, Cap, FixedProductTerms, Product, ScoreBasedProductTerms, Terms, TieredScoreBasedProductTerms,
            },
        },
        ConfigurableValue, Score, ValueTier,
    };
    use sweat_jar_primitives::UDecimal;

    use super::engine;

    const DEFAULT_STATE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test_data/account_full_state_190375496.json"
    );
    const DEFAULT_INTERACTIONS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_data/jar_interactions.csv");
    const DEFAULT_STEPS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../test_data/steps.csv");
    /// End of the replay window (ms): 2026-08-31T11:10:57.961Z.
    const DEFAULT_WINDOW_END_MS: u64 = 1_788_174_657_961;

    const YOCTO: u128 = 1_000_000_000_000_000_000;
    /// 365 days in ms — every product on this account has a 1-year lockup term.
    const LOCKUP_TERM_MS: u64 = 31_536_000_000;

    // ---------------------------------------------------------------------------
    // Product catalogue
    //
    // Terms mirror `products_referenced` in the full-state snapshot. Public keys
    // are intentionally dropped: the replay submits unsigned deposits, and we're
    // exercising interest math, not signature verification.
    // ---------------------------------------------------------------------------

    fn fixed(id: &str, apy_significand: u128, apy_exponent: u32, cap_min: u128, cap_max: u128) -> Product {
        Product {
            id: id.to_string(),
            cap: Cap::new(cap_min, cap_max),
            terms: Terms::Fixed(FixedProductTerms {
                lockup_term: LOCKUP_TERM_MS.into(),
                apy: Apy::Constant(UDecimal::new(apy_significand, apy_exponent)),
            }),
            withdrawal_fee: None,
            public_key: None,
            is_enabled: true,
        }
    }

    fn catalogue() -> Vec<Product> {
        vec![
            Product {
                id: "steps_365d_20000_score_cap".to_string(),
                cap: Cap::new(YOCTO, 500_000 * YOCTO),
                terms: Terms::ScoreBased(ScoreBasedProductTerms {
                    score_cap: 20_000,
                    lockup_term: LOCKUP_TERM_MS.into(),
                }),
                withdrawal_fee: None,
                public_key: None,
                is_enabled: true,
            },
            Product {
                id: "steps_365d_20000_10000_tiered_v1".to_string(),
                cap: Cap::new(YOCTO, 500_000 * YOCTO),
                terms: Terms::TieredScoreBased(TieredScoreBasedProductTerms {
                    score_cap: ConfigurableValue::Tier(ValueTier {
                        default: 20_000,
                        fallback: 10_000,
                    }),
                    lockup_term: LOCKUP_TERM_MS.into(),
                }),
                withdrawal_fee: None,
                public_key: None,
                is_enabled: true,
            },
            fixed("365d_12apy", 12, 2, YOCTO, 500_000 * YOCTO),
            fixed("365_14apy_upland", 14, 2, 1_000, 1_000 * YOCTO),
        ]
    }

    // ---------------------------------------------------------------------------
    // Snapshot parsing
    // ---------------------------------------------------------------------------

    struct Snapshot {
        account_id: AccountId,
        block_time_ms: u64,
        account: Account,
    }

    fn parse_snapshot(path: &str) -> Snapshot {
        let raw = fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read state file {path}: {e}"));
        let json: Value = serde_json::from_str(&raw).expect("state file is not valid JSON");

        let account_id: AccountId = json["account_id"].as_str().expect("account_id").parse().unwrap();
        let block_time_ms = json["block_time_utc"]
            .as_str()
            .map(parse_utc_datetime)
            .expect("block_time_utc");

        let account = engine::parse_account_state(&json["account_state"], block_time_ms);

        Snapshot {
            account_id,
            block_time_ms,
            account,
        }
    }

    // ---------------------------------------------------------------------------
    // Timeline
    // ---------------------------------------------------------------------------

    fn parse_interactions(path: &str) -> Vec<(u64, engine::Action)> {
        let raw = fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read interactions file {path}: {e}"));
        let mut events = Vec::new();

        for line in raw.lines().skip(1).filter(|l| !l.trim().is_empty()) {
            let cols: Vec<&str> = line.split(',').collect();
            // utc_time, block, type, method, product_id, amount_sweat, success, tx_hash
            let ts = parse_utc_datetime(cols[0]);
            let kind = cols[2];
            let method = cols.get(3).copied().unwrap_or("");
            let product_id = cols.get(4).copied().unwrap_or("");
            let amount = cols.get(5).copied().unwrap_or("");
            let success = cols.get(6).copied().unwrap_or("").eq_ignore_ascii_case("true");

            if !success {
                continue;
            }

            let action = match kind {
                "claim" => engine::Action::Claim,
                "deposit" => engine::Action::Deposit {
                    product_id: product_id.to_string(),
                    amount: parse_sweat_to_yocto(amount),
                },
                "subscription_activation" if method.contains("increased_score_cap=true") => {
                    engine::Action::SetIncreasedScoreCap(true)
                }
                "subscription_activation" if method.contains("increased_score_cap=false") => {
                    engine::Action::SetIncreasedScoreCap(false)
                }
                _ => continue,
            };

            events.push((ts, action));
        }

        events
    }

    fn parse_steps(path: &str) -> Vec<(u64, engine::Action)> {
        let raw = fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read steps file {path}: {e}"));
        raw.lines()
            .skip(1)
            .filter(|l| !l.trim().is_empty())
            .map(|line| {
                let (ts, steps) = line.split_once(',').expect("steps row");
                let ts: u64 = ts.trim().parse().expect("steps timestamp");
                let steps: u64 = steps.trim().parse().expect("steps value");
                let score = Score::try_from(steps).unwrap_or(Score::MAX);
                (ts, engine::Action::RecordScore(vec![(score, ts)]))
            })
            .collect()
    }

    // ---------------------------------------------------------------------------
    // Parsing helpers
    // ---------------------------------------------------------------------------

    /// Parses `YYYY-MM-DD HH:MM:SS` or `YYYY-MM-DDTHH:MM:SSZ` (UTC) to epoch ms.
    fn parse_utc_datetime(s: &str) -> u64 {
        let s = s.trim().trim_end_matches('Z');
        let (date, time) = s.split_once(['T', ' ']).expect("datetime separator");

        let mut d = date.split('-');
        let year: i64 = d.next().unwrap().parse().unwrap();
        let month: i64 = d.next().unwrap().parse().unwrap();
        let day: i64 = d.next().unwrap().parse().unwrap();

        let mut t = time.split(':');
        let hour: i64 = t.next().unwrap().parse().unwrap();
        let minute: i64 = t.next().unwrap().parse().unwrap();
        let second: i64 = t.next().unwrap_or("0").parse().unwrap();

        let days = days_from_civil(year, month, day);
        let secs = days * 86_400 + hour * 3_600 + minute * 60 + second;
        u64::try_from(secs).unwrap() * 1_000
    }

    /// Days since 1970-01-01 (Howard Hinnant's algorithm).
    fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// `"1654.88"` -> yocto (`* 10^18`), full precision, truncating beyond 18 digits.
    fn parse_sweat_to_yocto(s: &str) -> u128 {
        let s = s.trim();
        let (int_part, frac_part) = s.split_once('.').unwrap_or((s, ""));
        let int: u128 = if int_part.is_empty() {
            0
        } else {
            int_part.parse().unwrap()
        };

        let mut frac = frac_part.to_string();
        frac.truncate(18);
        while frac.len() < 18 {
            frac.push('0');
        }
        let frac: u128 = frac.parse().unwrap();

        int * YOCTO + frac
    }

    // ---------------------------------------------------------------------------
    // Test
    // ---------------------------------------------------------------------------

    fn path_from_env(var: &str, default: &str) -> String {
        std::env::var(var).unwrap_or_else(|_| default.to_string())
    }

    /// Pinned replay total: fails loudly if an engine change alters replay behavior.
    ///
    /// This test runs both as a plain `#[cfg(test)]` unit test (default
    /// features — `AccountScore::shift()`/`wipe()` stamp `updated_at`, the
    /// v4.2.3 fix) and under `--features replay-engine` (host build for the
    /// `replay/` crate — `shift()`/`wipe()` intentionally revert that fix to
    /// reproduce real pre-v4.2.3 chain behavior; see `model/src/data/score/mod.rs`).
    /// The two configurations replay different logic, so they pin different
    /// totals. This fixture account claims on nearly every occasion (172
    /// claims), so it hits the claim-races-oracle condition often; the ~93%
    /// drop under `replay-engine` is the cumulative effect across its full
    /// history, not a per-claim anomaly (single claims on real accounts move
    /// by ~0.1-1%, see the `replay/` reconciliation reports).
    #[cfg(any(not(feature = "replay-engine"), feature = "corrected-score-window"))]
    const GOLDEN_TOTAL_CLAIMED: u128 = 430_841_686_064_034_204_316_387;
    #[cfg(all(feature = "replay-engine", not(feature = "corrected-score-window")))]
    const GOLDEN_TOTAL_CLAIMED: u128 = 31_468_050_947_902_862_459_461;

    #[test]
    fn replay_account_history() {
        let window_end: u64 = std::env::var("JAR_REPLAY_END")
            .ok()
            .map(|v| v.trim().parse().expect("JAR_REPLAY_END must be epoch ms"))
            .unwrap_or(DEFAULT_WINDOW_END_MS);

        let snapshot = parse_snapshot(&path_from_env("JAR_REPLAY_STATE", DEFAULT_STATE));
        let mut parsed = parse_interactions(&path_from_env("JAR_REPLAY_INTERACTIONS", DEFAULT_INTERACTIONS));
        parsed.extend(parse_steps(&path_from_env("JAR_REPLAY_STEPS", DEFAULT_STEPS)));

        // Replay window: [snapshot block time, window_end]. `seq` keeps file order as
        // the final tie-break inside `Timeline::sorted`.
        let mut events: Vec<engine::Event> = Vec::new();
        for (seq, (ts_ms, action)) in parsed.into_iter().enumerate() {
            if ts_ms >= snapshot.block_time_ms && ts_ms <= window_end {
                events.push(engine::Event {
                    ts_ms,
                    seq: seq as u64,
                    action,
                });
            }
        }

        let timeline = engine::Timeline { events }.sorted();

        let outcome = engine::run_timeline(
            engine::Baseline {
                account_id: snapshot.account_id.clone(),
                raw_account: Some(to_vec(&AccountVersioned::new(snapshot.account)).unwrap()),
                timezone_ms: None,
            },
            &catalogue(),
            snapshot.block_time_ms,
            timeline,
        );

        for (ts, amount) in &outcome.per_claim {
            println!("{ts} - {amount}");
        }
        println!("---");
        println!("claims: {}", outcome.per_claim.len());
        println!("TOTAL CLAIMED: {}", outcome.total_claimed);

        assert!(matches!(outcome.status, engine::ReplayStatus::Ok));
        assert!(
            outcome.total_claimed > 0,
            "expected the account to have claimed something"
        );
        assert_eq!(
            outcome.total_claimed, GOLDEN_TOTAL_CLAIMED,
            "golden replay total changed"
        );
    }
}
