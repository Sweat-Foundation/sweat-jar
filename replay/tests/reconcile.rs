//! `reconcile_user` over a DuckDB fixture built from the parquet dataset.

mod common;
use common::write_fixture_dataset;
use replay::db;
use replay::db::ingest::{build_db, BuildOpts};
use replay::products::load_products;
use replay::reconcile::reconcile_user;
use replay::snapshot::{DbSnapshotSource, SnapshotSource};

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
