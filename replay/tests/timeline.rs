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
            Action::Claim { .. } => "claim",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["deposit", "score", "booster", "claim"]);

    // The fixture's record_score payload timestamp (1774065600000) is the
    // *Local* value `record_score`'s own event would emit for a UTC-5
    // account (`ScoreData.score: Vec<(Score, Local)>`) — not the raw UTC the
    // oracle submitted. `load_user` must subtract the account's timezone
    // (-18_000_000ms = -5h) to recover that raw UTC before it's replayed
    // (the engine's `record_score` call re-applies the shift on the way
    // in); feeding the Local value straight back in would double-apply it
    // and can bucket the step into the wrong calendar day. Recovering
    // 1774065600000 - (-18_000_000) = 1774083600000 exceeds this event's own
    // block time (1774072800000), so the existing future-clamp caps it
    // there — this is the value the engine actually replays.
    let Action::RecordScore(pairs) = &tl.events[1].action else {
        panic!("expected RecordScore at index 1, got {:?}", tl.events[1].action);
    };
    assert_eq!(pairs, &vec![(9000, 1_774_072_800_000)]);
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
fn deposit_and_apply_booster_sharing_a_block_merge_into_airdrop_with_booster() {
    let d = tempfile::tempdir().unwrap();
    let c = conn(d.path());
    let (_slice, tl) = load_user(&c, 700).unwrap();

    // Deposit (log_index 0) and apply_booster (log_index 1) shared one
    // block_height in the fixture — they must merge into a single action,
    // not replay as two independent events. Only the trailing claim remains
    // separate.
    assert_eq!(tl.events.len(), 2, "expected [AirdropWithBooster, Claim], got {:?}", tl.events);
    assert!(
        matches!(
            tl.events[0].action,
            Action::AirdropWithBooster { ref product_id, amount: 1_000_000_000_000_000_000_000, score: 3000, .. }
            if product_id == "steps_365d_20000_10000_tiered_v1"
        ),
        "expected AirdropWithBooster at index 0, got {:?}",
        tl.events[0].action
    );
    assert!(matches!(tl.events[1].action, Action::Claim { .. }));
}

#[test]
fn unknown_account_is_err() {
    let d = tempfile::tempdir().unwrap();
    let c = conn(d.path());
    assert!(load_user(&c, 999).is_err());
}
