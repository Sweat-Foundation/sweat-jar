//! `timeline::load_user` against a fixture DuckDB built by `build_db`.

mod common;
use common::write_fixture_dataset;
use replay::db;
use replay::db::ingest::{build_db, BuildOpts};
use replay::timeline::load_user;
use sweat_jar::replay::engine::Action;

fn conn(dir: &std::path::Path) -> duckdb::Connection {
    let src = dir.join("src");
    write_fixture_dataset(&src);
    let dbp = dir.join("t.duckdb");
    let mut c = db::open_write(&dbp).unwrap();
    db::schema::init_schema(&c).unwrap();
    build_db(&mut c, &BuildOpts { source_dir: &src, accounts: None, sample: None }).unwrap();
    c
}

#[test]
fn account_200_maps_all_event_kinds() {
    let d = tempfile::tempdir().unwrap();
    let c = conn(d.path());
    let (slice, tl) = load_user(&c, 200).unwrap();
    assert_eq!(slice.near_account_id, "near200");
    assert!(!slice.existed_at_start);
    assert_eq!(slice.timezone_ms, Some(-18_000_000));
    assert_eq!(slice.onchain_claimed, 123);

    let kinds: Vec<&str> = tl
        .events
        .iter()
        .map(|e| match &e.action {
            Action::Deposit { .. } => "deposit",
            Action::RecordScore(_) => "score",
            Action::ApplyBooster { .. } => "booster",
            Action::Claim => "claim",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["deposit", "score", "booster", "claim"]);
}

#[test]
fn account_300_withdraw_all_and_empty_score_skipped() {
    let d = tempfile::tempdir().unwrap();
    let c = conn(d.path());
    let (_slice, tl) = load_user(&c, 300).unwrap();
    // record_score '[]' produces no event; deposit + withdraw_all remain.
    assert_eq!(tl.events.len(), 2);
    assert!(matches!(tl.events[1].action, Action::WithdrawAll { ref product_ids }
        if product_ids == &vec!["365d_12apy".to_string()]));
}

#[test]
fn single_jar_restake_keeps_source_and_target_products() {
    let d = tempfile::tempdir().unwrap();
    let c = conn(d.path());
    let (_slice, tl) = load_user(&c, 400).unwrap();
    let restake = tl
        .events
        .iter()
        .find(|e| matches!(e.action, Action::Restake { .. }))
        .expect("restake action");
    assert!(matches!(restake.action, Action::Restake { ref from, ref into, amount: 7 }
        if from == "365d_12apy" && into == "steps_365d_20000_10000_tiered_v1"));
}

#[test]
fn multi_jar_restake_becomes_restake_all() {
    let d = tempfile::tempdir().unwrap();
    let c = conn(d.path());
    let (_slice, tl) = load_user(&c, 500).unwrap();
    assert_eq!(tl.events.len(), 1);
    assert!(matches!(tl.events[0].action, Action::RestakeAll { ref product_id, amount: 9 }
        if product_id == "365d_12apy"));
}

#[test]
fn future_increment_timestamps_are_clamped_to_block_time() {
    let d = tempfile::tempdir().unwrap();
    let c = conn(d.path());
    let (_slice, tl) = load_user(&c, 400).unwrap();

    let score = tl
        .events
        .iter()
        .find(|e| matches!(e.action, Action::RecordScore(_)))
        .expect("record_score action");
    let Action::RecordScore(ref pairs) = score.action else { unreachable!() };
    // First increment was dated in the future -> clamped; second was already in the past.
    assert_eq!(pairs[0], (9000, score.ts_ms));
    assert_eq!(pairs[1], (1000, 1_774_054_800_000));

    let booster = tl
        .events
        .iter()
        .find(|e| matches!(e.action, Action::ApplyBooster { .. }))
        .expect("apply_booster action");
    assert!(matches!(booster.action, Action::ApplyBooster { score: 3000, timestamp_ms }
        if timestamp_ms == booster.ts_ms));
}

#[test]
fn unknown_account_is_err() {
    let d = tempfile::tempdir().unwrap();
    let c = conn(d.path());
    assert!(load_user(&c, 999).is_err());
}
