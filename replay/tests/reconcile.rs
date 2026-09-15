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

/// Answers `raw_account_at` for BOTH the pre-event and the post-event block
/// height, with different content — the fixture's stand-in for a multi-receipt
/// operation (lock jars, promise, callback emits the event) whose pre-event
/// block can show a transient mid-flight snapshot of that very same
/// operation, not a genuinely separate prior state.
struct BothHeightsAnswered {
    pre_event_block_height: u64,
    pre_event_state_json: &'static str,
    post_event_block_height: u64,
    post_event_state_json: &'static str,
}

impl SnapshotSource for BothHeightsAnswered {
    fn raw_account(&self, _account_id: i64, _near_account_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn is_authoritative(&self) -> bool {
        true
    }
    fn raw_account_at(&self, _near_account_id: &str, block_height: u64) -> anyhow::Result<Option<Vec<u8>>> {
        if block_height == self.pre_event_block_height {
            Ok(Some(account_state_json_to_raw(self.pre_event_state_json)?))
        } else if block_height == self.post_event_block_height {
            Ok(Some(account_state_json_to_raw(self.post_event_state_json)?))
        } else {
            Ok(None)
        }
    }
}

#[test]
fn non_deposit_first_action_never_checks_the_pre_event_block() {
    // account 500: first tracked event is a multi-jar `restake` (not a
    // `Deposit`) at block_height 190000800 — a multi-receipt operation on
    // the real chain (lock jars, withdrawal promise, callback emits the
    // event). If the fallback checked the pre-event block for a non-Deposit
    // first action, it could pick up a transient locked snapshot of that
    // very same restake instead of a genuinely separate prior state
    // (verified against a real production account: `is_pending_withdraw:
    // true` on the jars the restake was about to consume, one block before
    // its own log event) — silently corrupting the baseline. Both heights
    // answer here with different, distinguishable content; only the
    // post-event one must ever be used.
    let d = tempfile::tempdir().unwrap();
    let dbp = build_fixture_db(d.path());
    let c = db::open_read(&dbp).unwrap();

    let snap = BothHeightsAnswered {
        pre_event_block_height: 190_000_799,
        pre_event_state_json: r#"{"nonce":0,"jars":{"365d_12apy":{"deposits":[["1774000000000","1"]],"cache":null,"is_pending_withdraw":true,"claim_remainder":"0"},"90d_3apy":{"deposits":[["1774000000000","1"]],"cache":null,"is_pending_withdraw":true,"claim_remainder":"0"}},"score":{"updated_at":1774000000000,"history":[]},"is_penalty_applied":false,"features":{},"timezone":0}"#,
        post_event_block_height: 190_000_800,
        post_event_state_json: r#"{"nonce":1,"jars":{"365d_12apy":{"deposits":[["1774000000000","9"]],"cache":null,"is_pending_withdraw":false,"claim_remainder":"0"}},"score":{"updated_at":1774000000000,"history":[]},"is_penalty_applied":false,"features":{},"timezone":0}"#,
    };

    let row = reconcile_user(&c, 500, &products(), &snap).unwrap();
    // If the pre-event (locked, mid-flight) snapshot had been used as the
    // baseline AND the restake replayed on top of it, this would panic as
    // "error: Not enough funds to restake" (reproduced against the real
    // account this scenario is modeled on). Landing on "ok" proves only the
    // post-event height was ever consulted.
    assert_eq!(row.status, "ok", "row: {row:?}");
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
