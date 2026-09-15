//! `reconcile_user` over a DuckDB fixture built from the parquet dataset.

mod common;
use common::write_fixture_dataset;
use replay::db;
use replay::db::ingest::{build_db, BuildOpts};
use replay::products::load_products;
use replay::reconcile::{apply_block_height_fallback, reconcile_user};
use replay::snapshot::{account_state_json_to_raw, DbSnapshotSource, SnapshotSource};
use replay::timeline::UserSlice;
use sweat_jar::replay::engine::{Action, Event, Timeline};

fn build_fixture_db(dir: &std::path::Path) -> std::path::PathBuf {
    let src = dir.join("src");
    write_fixture_dataset(&src);
    let dbp = dir.join("r.duckdb");
    let mut c = db::open_write(&dbp).unwrap();
    db::schema::init_schema(&c).unwrap();
    build_db(&mut c, &BuildOpts { source_dir: &src, accounts: None, sample: None }).unwrap();
    drop(c);
    dbp
}

fn products() -> Vec<sweat_jar_model::data::product::Product> {
    load_products(std::path::Path::new("tests/fixtures/products.json")).unwrap()
}

#[test]
fn fresh_account_reconciles_without_baseline_flag() {
    let d = tempfile::tempdir().unwrap();
    let dbp = build_fixture_db(d.path());
    let c = db::open_read(&dbp).unwrap();
    let snap = DbSnapshotSource::new(&dbp);

    // account 300: fresh (existed_at_start = false), deposit -> withdraw_all.
    let row = reconcile_user(&c, 300, &products(), &snap).unwrap();
    assert_eq!(row.status, "ok", "row: {row:?}");
}

#[test]
fn existed_at_start_account_without_snapshot_is_no_baseline() {
    let d = tempfile::tempdir().unwrap();
    let dbp = build_fixture_db(d.path());
    let c = db::open_read(&dbp).unwrap();
    let snap = DbSnapshotSource::new(&dbp);

    // account 100: existed_at_start = true, no events, no snapshot row.
    let row = reconcile_user(&c, 100, &products(), &snap).unwrap();
    assert_eq!(row.status, "no_baseline", "row: {row:?}");
    assert_eq!(row.n_claims, 0, "row: {row:?}");
}

/// Always answers "confirmed no state" — stands in for a real archival lookup
/// that came back `null` (the account genuinely had no state at H).
struct AlwaysAuthoritativeEmpty;

impl SnapshotSource for AlwaysAuthoritativeEmpty {
    fn raw_account(&self, _account_id: i64, _near_account_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn is_authoritative(&self) -> bool {
        true
    }
}

#[test]
fn existed_at_start_account_confirmed_empty_by_an_authoritative_source_is_ok_not_no_baseline() {
    let d = tempfile::tempdir().unwrap();
    let dbp = build_fixture_db(d.path());
    let c = db::open_read(&dbp).unwrap();

    // account 100: existed_at_start = true, but an authoritative source (e.g.
    // archival) confirming no state at H is a complete answer, not a gap —
    // `existed_at_start` is the export's own classification, not fetched
    // ground truth, so it must not override what the source actually says.
    let row = reconcile_user(&c, 100, &products(), &AlwaysAuthoritativeEmpty).unwrap();
    assert_eq!(row.status, "ok", "row: {row:?}");
}

/// Stands in for a real archival RPC failure (timeout, throttling, malformed
/// response) — an infra problem, not a per-account contract divergence.
struct AlwaysFails;

impl SnapshotSource for AlwaysFails {
    fn raw_account(&self, _account_id: i64, _near_account_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        anyhow::bail!("simulated RPC timeout")
    }
    fn is_authoritative(&self) -> bool {
        true
    }
}

#[test]
fn snapshot_fetch_error_is_a_failure_not_an_error() {
    let d = tempfile::tempdir().unwrap();
    let dbp = build_fixture_db(d.path());
    let c = db::open_read(&dbp).unwrap();

    // A fetch that errors out never even reached this account's contract
    // logic — that's a `failure:`, not an `error:` (see reconcile_user's doc:
    // error: = the contract logic ran and panicked; failure: = we never got a
    // clean read).
    let row = reconcile_user(&c, 100, &products(), &AlwaysFails).unwrap();
    assert!(row.status.starts_with("failure:"), "row: {row:?}");
    assert!(!row.status.starts_with("error:"), "row: {row:?}");
}

#[test]
fn contract_panic_is_an_error_not_a_failure() {
    let d = tempfile::tempdir().unwrap();
    let dbp = build_fixture_db(d.path());
    let c = db::open_read(&dbp).unwrap();
    let snap = DbSnapshotSource::new(&dbp);

    // account 400: restakes from a jar this fixture never deposited into —
    // the fetch succeeds cleanly, the engine runs, and the CONTRACT panics
    // given this account's (synthetic) history. That's an `error:`, not a
    // `failure:` — rerunning it changes nothing.
    let row = reconcile_user(&c, 400, &products(), &snap).unwrap();
    assert!(row.status.starts_with("error:"), "row: {row:?}");
    assert!(!row.status.starts_with("failure:"), "row: {row:?}");
}

/// Confirms "no state at H" (like a real archival `null`), but additionally
/// answers `raw_account_at` for one specific block — the fixture's stand-in
/// for a real archival node that can serve any historical height.
struct FallbackOnly {
    block_height: u64,
    state_json: &'static str,
}

impl SnapshotSource for FallbackOnly {
    fn raw_account(&self, _account_id: i64, _near_account_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn is_authoritative(&self) -> bool {
        true
    }
    fn raw_account_at(&self, _near_account_id: &str, block_height: u64) -> anyhow::Result<Option<Vec<u8>>> {
        if block_height == self.block_height {
            Ok(Some(replay::snapshot::account_state_json_to_raw(self.state_json)?))
        } else {
            Ok(None)
        }
    }
}

#[test]
fn account_not_found_via_invisible_creation_recovers_via_block_height_fallback() {
    let d = tempfile::tempdir().unwrap();
    let dbp = build_fixture_db(d.path());
    let c = db::open_read(&dbp).unwrap();

    // account 600: first event is a `claim` (no deposit anywhere in the
    // export) at block 190000900 — stands in for an FT-transfer migration
    // that wrote the account's storage directly with no emitted event. The
    // block-H baseline is confirmed empty by an authoritative source, so the
    // engine re-fetches state at that block instead of erroring "not found".
    let snap = FallbackOnly {
        block_height: 190_000_900,
        state_json: r#"{"nonce":1,"jars":{"365d_12apy":{"deposits":[["1774000000000","1000000000000000000000"]],"cache":{"updated_at":"1774483200000","interest":"0"},"is_pending_withdraw":false,"claim_remainder":"0"}},"score":{"updated_at":1774483200000,"history":[]},"is_penalty_applied":false,"features":{},"timezone":0}"#,
    };

    let row = reconcile_user(&c, 600, &products(), &snap).unwrap();
    assert_eq!(row.status, "ok", "row: {row:?}");
    // The dropped claim (amount 5) is already reflected in the recovered
    // baseline — it must come out of `actual_total_claim` too, or this
    // account would show a spurious 100% divergence (calculated 0 vs
    // on-chain 5) despite reconciling cleanly.
    assert_eq!(row.actual_total_claim, "0", "row: {row:?}");
}

/// Confirms "no state at H", but also answers `raw_account_at` for one block
/// height BEFORE the account's first tracked event — the fixture's stand-in
/// for a real archival node revealing a pre-existing account created by an
/// untracked event (e.g. a migration) sometime within the replay window,
/// before the first event this export happens to track.
struct PreEventStateOnly {
    pre_event_block_height: u64,
    state_json: &'static str,
}

impl SnapshotSource for PreEventStateOnly {
    fn raw_account(&self, _account_id: i64, _near_account_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn is_authoritative(&self) -> bool {
        true
    }
    fn raw_account_at(&self, _near_account_id: &str, block_height: u64) -> anyhow::Result<Option<Vec<u8>>> {
        if block_height == self.pre_event_block_height {
            Ok(Some(account_state_json_to_raw(self.state_json)?))
        } else {
            Ok(None)
        }
    }
}

#[test]
fn deposit_first_account_with_invisible_pre_existing_state_recovers_via_block_height_fallback() {
    // A Deposit-first account — exactly the case the old (narrower) fallback
    // skipped entirely, trusting that a leading Deposit always means a
    // genuinely fresh account. Here the account already held an untracked
    // jar one block before this Deposit, simulating a migration that
    // happened invisibly earlier in the window.
    let slice = UserSlice {
        backend_account_id: 9001,
        near_account_id: "pre-existing.near".to_string(),
        existed_at_start: false,
        timezone_ms: Some(0),
        onchain_claimed: 0,
        first_event_block_height: Some(190_000_500),
        first_event_claim_amount: None,
        migration: None,
    };
    let timeline = Timeline {
        events: vec![Event {
            ts_ms: 1_774_000_100_000,
            seq: 0,
            action: Action::Deposit { product_id: "365d_12apy".to_string(), amount: 1_000_000_000_000_000_000_000 },
        }],
    };

    let pre_existing_state = r#"{"nonce":1,"jars":{"90d_3apy":{"deposits":[["1700000000000","500000000000000000000"]],"cache":{"updated_at":"1774000000000","interest":"0"},"is_pending_withdraw":false,"claim_remainder":"0"}},"score":{"updated_at":1774000000000,"history":[]},"is_penalty_applied":false,"features":{},"timezone":0}"#;
    let expected_bytes = account_state_json_to_raw(pre_existing_state).unwrap();

    let snap = PreEventStateOnly { pre_event_block_height: 190_000_499, state_json: pre_existing_state };

    let (raw_account, out_timeline, claimed_adjustment) =
        apply_block_height_fallback(&snap, &slice, None, timeline);

    assert_eq!(raw_account, Some(expected_bytes), "pre-existing state must be used as the baseline");
    // The Deposit is NOT dropped — its own effect still needs replaying on
    // top of the recovered baseline (unlike the post-event-fetch case,
    // where the leading event's effect is already reflected in the state).
    assert_eq!(out_timeline.events.len(), 1, "the Deposit must still be replayed, not dropped");
    assert_eq!(claimed_adjustment, 0);
}

/// Answers `raw_account_at` for a fixed set of heights with distinguishable
/// content — the fixture's stand-in for a multi-receipt operation (lock
/// jars, promise, callback emits the event) whose immediately-preceding
/// blocks can show transient mid-flight snapshots of that very same
/// operation, resolving to the true prior state only a few blocks earlier.
struct HeightsAnswered {
    answers: Vec<(u64, &'static str)>,
}

impl SnapshotSource for HeightsAnswered {
    fn raw_account(&self, _account_id: i64, _near_account_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn is_authoritative(&self) -> bool {
        true
    }
    fn raw_account_at(&self, _near_account_id: &str, block_height: u64) -> anyhow::Result<Option<Vec<u8>>> {
        match self.answers.iter().find(|(h, _)| *h == block_height) {
            Some((_, json)) => Ok(Some(account_state_json_to_raw(json)?)),
            None => Ok(None),
        }
    }
}

const LOCKED_STATE_JSON: &str = r#"{"nonce":0,"jars":{"365d_12apy":{"deposits":[["1774000000000","1"]],"cache":null,"is_pending_withdraw":true,"claim_remainder":"0"},"90d_3apy":{"deposits":[["1774000000000","1"]],"cache":null,"is_pending_withdraw":true,"claim_remainder":"0"}},"score":{"updated_at":1774000000000,"history":[]},"is_penalty_applied":false,"features":{},"timezone":0}"#;

#[test]
fn non_deposit_first_action_never_uses_a_locked_snapshot() {
    // account 500: first tracked event is a multi-jar `restake` (not a
    // `Deposit`) at block_height 190000800 — a multi-receipt operation on
    // the real chain (lock jars, withdrawal promise, callback emits the
    // event). The block right before it answers with a locked (mid-flight)
    // snapshot of that very same restake (verified against a real
    // production account: `is_pending_withdraw: true` on the jars the
    // restake was about to consume, one block before its own log event).
    // Only the post-event block answers otherwise, so if the fallback used
    // the locked snapshot as the baseline instead of walking further back
    // or falling through to the post-event fetch, replaying the restake on
    // top of it would panic.
    let d = tempfile::tempdir().unwrap();
    let dbp = build_fixture_db(d.path());
    let c = db::open_read(&dbp).unwrap();

    let snap = HeightsAnswered {
        answers: vec![
            (190_000_799, LOCKED_STATE_JSON),
            (190_000_800, r#"{"nonce":1,"jars":{"365d_12apy":{"deposits":[["1774000000000","9"]],"cache":null,"is_pending_withdraw":false,"claim_remainder":"0"}},"score":{"updated_at":1774000000000,"history":[]},"is_penalty_applied":false,"features":{},"timezone":0}"#),
        ],
    };

    let row = reconcile_user(&c, 500, &products(), &snap).unwrap();
    // If the locked snapshot had been used as the baseline AND the restake
    // replayed on top of it, this would panic as "error: Not enough funds
    // to restake" (reproduced against the real account this scenario is
    // modeled on). Landing on "ok" proves it was never used as-is.
    assert_eq!(row.status, "ok", "row: {row:?}");
}

#[test]
fn walkback_skips_multiple_locked_snapshots_to_find_the_true_prior_state() {
    // Same account 500 restake, but now the true pre-restake state sits
    // THREE blocks back (190000797), with locked snapshots at both
    // intermediate heights (190000799, 190000798) — exactly the shape found
    // on the real production account this fallback is modeled on (locked at
    // offset 1 and 2, resolved at offset 3). Unlike the previous test, this
    // checks `apply_block_height_fallback` directly so the exact recovered
    // baseline and the untouched timeline can be asserted precisely.
    let d = tempfile::tempdir().unwrap();
    let dbp = build_fixture_db(d.path());
    let c = db::open_read(&dbp).unwrap();
    let (slice, timeline) = replay::timeline::load_user(&c, 500).unwrap();
    assert_eq!(timeline.events.len(), 1, "fixture account 500 should have exactly the one restake event");

    let true_prior_state = r#"{"nonce":0,"jars":{"365d_12apy":{"deposits":[["1774000000000","1"]],"cache":null,"is_pending_withdraw":false,"claim_remainder":"0"},"90d_3apy":{"deposits":[["1774000000000","1"]],"cache":null,"is_pending_withdraw":false,"claim_remainder":"0"}},"score":{"updated_at":1774000000000,"history":[]},"is_penalty_applied":false,"features":{},"timezone":0}"#;
    let expected_bytes = account_state_json_to_raw(true_prior_state).unwrap();

    let snap = HeightsAnswered {
        answers: vec![
            (190_000_799, LOCKED_STATE_JSON),
            (190_000_798, LOCKED_STATE_JSON),
            (190_000_797, true_prior_state),
        ],
    };

    let (raw_account, out_timeline, claimed_adjustment) =
        apply_block_height_fallback(&snap, &slice, None, timeline);

    assert_eq!(raw_account, Some(expected_bytes), "must walk back past both locked snapshots to the resolved state");
    assert_eq!(out_timeline.events.len(), 1, "the restake must still be replayed, not dropped");
    assert_eq!(claimed_adjustment, 0);
}

/// Panics if ever asked for account state at all — proves a code path never
/// touches the network.
struct PanicsIfCalled;

impl SnapshotSource for PanicsIfCalled {
    fn raw_account(&self, _account_id: i64, _near_account_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn is_authoritative(&self) -> bool {
        true
    }
    fn raw_account_at(&self, _near_account_id: &str, _block_height: u64) -> anyhow::Result<Option<Vec<u8>>> {
        panic!("raw_account_at must not be called when the migrations table already has the raw bytes")
    }
}

#[test]
fn migration_record_with_raw_bytes_bypasses_the_network_entirely() {
    // account 300: fresh, deposit -> withdraw_all. Insert a `migrations` row
    // for it directly (standing in for the `jars_merge_events` export,
    // which the shared test fixture doesn't carry) with the raw bytes
    // already present — the ~98% case where the export captured
    // `migrated_jars_borsh_base64`. This must be used as-is, with no
    // archival call at all.
    let d = tempfile::tempdir().unwrap();
    let dbp = build_fixture_db(d.path());
    let c = db::open_write(&dbp).unwrap();

    let migrated_state = r#"{"nonce":1,"jars":{"90d_3apy":{"deposits":[["1700000000000","500000000000000000000"]],"cache":{"updated_at":"1774000000000","interest":"0"},"is_pending_withdraw":false,"claim_remainder":"0"}},"score":{"updated_at":1774000000000,"history":[]},"is_penalty_applied":false,"features":{},"timezone":0}"#;
    let expected_bytes = account_state_json_to_raw(migrated_state).unwrap();
    c.execute(
        "INSERT INTO migrations VALUES (300, 190000000, ?)",
        duckdb::params![expected_bytes],
    )
    .unwrap();

    let (slice, timeline) = replay::timeline::load_user(&c, 300).unwrap();
    assert!(slice.migration.is_some(), "the inserted migration row must be loaded");

    let (raw_account, out_timeline, claimed_adjustment) =
        apply_block_height_fallback(&PanicsIfCalled, &slice, None, timeline);

    assert_eq!(raw_account, Some(expected_bytes));
    assert_eq!(out_timeline.events.len(), 2, "both events must still be replayed, not dropped");
    assert_eq!(claimed_adjustment, 0);
}

#[test]
fn migration_record_without_raw_bytes_does_one_direct_fetch_at_its_own_block() {
    // Same account 300, but the `migrations` row has no raw bytes (the ~2%
    // export gap) — a single fetch at the row's own `block_height` must be
    // used, with no walk-back (JarsMerge's block is never locked).
    let d = tempfile::tempdir().unwrap();
    let dbp = build_fixture_db(d.path());
    let c = db::open_write(&dbp).unwrap();

    c.execute(
        "INSERT INTO migrations VALUES (300, 190000000, NULL)",
        duckdb::params![],
    )
    .unwrap();

    let (slice, timeline) = replay::timeline::load_user(&c, 300).unwrap();
    let migration = slice.migration.as_ref().unwrap();
    assert_eq!(migration.block_height, 190_000_000);
    assert!(migration.raw_account.is_none());

    let migrated_state = r#"{"nonce":1,"jars":{"90d_3apy":{"deposits":[["1700000000000","500000000000000000000"]],"cache":{"updated_at":"1774000000000","interest":"0"},"is_pending_withdraw":false,"claim_remainder":"0"}},"score":{"updated_at":1774000000000,"history":[]},"is_penalty_applied":false,"features":{},"timezone":0}"#;
    let expected_bytes = account_state_json_to_raw(migrated_state).unwrap();
    let snap = PreEventStateOnly { pre_event_block_height: 190_000_000, state_json: migrated_state };

    let (raw_account, out_timeline, claimed_adjustment) =
        apply_block_height_fallback(&snap, &slice, None, timeline);

    assert_eq!(raw_account, Some(expected_bytes));
    assert_eq!(out_timeline.events.len(), 2, "both events must still be replayed, not dropped");
    assert_eq!(claimed_adjustment, 0);
}

#[test]
fn fresh_account_with_claim_reconciles_ok() {
    let d = tempfile::tempdir().unwrap();
    let dbp = build_fixture_db(d.path());
    let c = db::open_read(&dbp).unwrap();
    let snap = DbSnapshotSource::new(&dbp);

    // account 200: fresh, tiered-score deposit + record_score + booster + claim(123).
    let row = reconcile_user(&c, 200, &products(), &snap).unwrap();
    assert_eq!(row.status, "ok", "row: {row:?}");
    assert!(row.n_claims >= 1, "row: {row:?}");
    assert_eq!(row.actual_total_claim, "123", "row: {row:?}");
}
