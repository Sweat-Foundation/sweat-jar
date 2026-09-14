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
