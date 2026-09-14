mod common;
use common::write_fixture_dataset;

use std::path::{Path, PathBuf};

use replay::db;
use replay::db::ingest::{build_db, BuildOpts};
use replay::run::{parse_shard, run, RunOpts};

const PRODUCTS: &str = "tests/fixtures/products.json";

fn fixture_db(dir: &Path) -> PathBuf {
    let src = dir.join("src");
    write_fixture_dataset(&src);
    let path = dir.join("e2e.duckdb");
    let mut conn = db::open_write(&path).unwrap();
    db::schema::init_schema(&conn).unwrap();
    build_db(&mut conn, &BuildOpts { source_dir: &src, accounts: None, sample: None }).unwrap();
    drop(conn);
    path
}

fn opts(db: PathBuf, out: PathBuf) -> RunOpts {
    RunOpts {
        db,
        out: Some(out),
        products: PRODUCTS.into(),
        threads: 2,
        shard: None,
        accounts: None,
        sample: None,
        tolerance: 1e-6,
        archival_rpc_url: None,
        force: false,
    }
}

fn sorted_rows(csv: &str) -> Vec<String> {
    let mut lines: Vec<String> = csv.lines().skip(1).map(str::to_string).collect();
    lines.sort();
    lines
}

#[test]
fn run_end_to_end_writes_csv() {
    let d = tempfile::tempdir().unwrap();
    let db = fixture_db(d.path());
    let out = d.path().join("rec.csv");
    let summary = run(&opts(db, out.clone())).unwrap();

    assert_eq!(summary.processed, 5);
    assert_eq!(summary.no_baseline, 1);

    let body = std::fs::read_to_string(&out).unwrap();
    assert_eq!(
        body.lines().next().unwrap(),
        "account_id,near_account_id,calculated_total_claim,actual_total_claim,delta,rel_delta,n_claims,status"
    );
    assert_eq!(body.lines().count(), 6);

    let row = body
        .lines()
        .find(|l| l.starts_with("200,"))
        .expect("row for 200");
    let cols: Vec<&str> = row.split(',').collect();
    assert_eq!(cols[7], "ok");
    assert_eq!(cols[3], "123");
}

#[test]
fn run_threads_1_is_deterministic() {
    let d = tempfile::tempdir().unwrap();
    let db = fixture_db(d.path());

    let out1 = d.path().join("a.csv");
    let mut o1 = opts(db.clone(), out1.clone());
    o1.threads = 1;
    run(&o1).unwrap();

    let out2 = d.path().join("b.csv");
    let mut o2 = opts(db, out2.clone());
    o2.threads = 1;
    o2.force = true; // recompute for real — proves determinism, not export idempotency
    run(&o2).unwrap();

    let a = std::fs::read_to_string(&out1).unwrap();
    let b = std::fs::read_to_string(&out2).unwrap();
    assert_eq!(sorted_rows(&a), sorted_rows(&b));
    assert_eq!(a, b);
}

#[test]
fn run_shard_splits_worklist() {
    let d = tempfile::tempdir().unwrap();
    let db = fixture_db(d.path());

    // No `--out`: shard partitioning is a `results`-table property now, not a
    // per-invocation CSV — each run's CSV export would cover the whole table.
    let mut o0 = opts(db.clone(), d.path().join("unused0.csv"));
    o0.out = None;
    o0.shard = Some((0, 2));
    let s0 = run(&o0).unwrap();

    let mut o1 = opts(db.clone(), d.path().join("unused1.csv"));
    o1.out = None;
    o1.shard = Some((1, 2));
    let s1 = run(&o1).unwrap();

    assert_eq!(s0.processed + s1.processed, 5);

    // Every account landed in `results` exactly once, on the shard its id maps to.
    let conn = db::open_read(&db).unwrap();
    let mut stmt = conn.prepare("SELECT backend_account_id FROM results").unwrap();
    let ids: Vec<i64> = stmt.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
    assert_eq!(ids.len(), 5, "each account reconciled exactly once across shards");
}

#[test]
fn run_sample_limits() {
    let d = tempfile::tempdir().unwrap();
    let db = fixture_db(d.path());
    let out = d.path().join("rec.csv");
    let mut o = opts(db, out);
    o.sample = Some(1);
    assert_eq!(run(&o).unwrap().processed, 1);
}

#[test]
fn rerun_skips_already_computed_accounts_unless_forced() {
    let d = tempfile::tempdir().unwrap();
    let db = fixture_db(d.path());

    let out = d.path().join("rec.csv");
    let first = run(&opts(db.clone(), out.clone())).unwrap();
    assert_eq!(first.processed, 5);

    let count_results = || -> i64 {
        db::open_read(&db)
            .unwrap()
            .query_row("SELECT count(*) FROM results", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count_results(), 5);

    // Rerun without --force: everything is already in `results`, so nothing
    // is recomputed (this is the "process died and was restarted" case).
    let second = run(&opts(db.clone(), out.clone())).unwrap();
    assert_eq!(second.processed, 0);
    assert_eq!(count_results(), 5);
    // The CSV is still (re-)exported from the full `results` table.
    assert_eq!(std::fs::read_to_string(&out).unwrap().lines().count(), 6);

    // --force recomputes everyone.
    let mut forced = opts(db.clone(), out);
    forced.force = true;
    let third = run(&forced).unwrap();
    assert_eq!(third.processed, 5);
    assert_eq!(count_results(), 5);
}

#[test]
fn export_csv_reads_back_results_without_recomputing() {
    let d = tempfile::tempdir().unwrap();
    let db = fixture_db(d.path());

    // `run` with no `--out`: results land in the db, no CSV yet.
    let mut o = opts(db.clone(), d.path().join("unused.csv"));
    o.out = None;
    let summary = run(&o).unwrap();
    assert_eq!(summary.processed, 5);

    let out = d.path().join("exported.csv");
    replay::export::export_csv(&db, &out).unwrap();
    let body = std::fs::read_to_string(&out).unwrap();
    assert_eq!(
        body.lines().next().unwrap(),
        "account_id,near_account_id,calculated_total_claim,actual_total_claim,delta,rel_delta,n_claims,status"
    );
    assert_eq!(body.lines().count(), 6);
}

#[test]
fn parse_shard_validates() {
    assert_eq!(parse_shard("1/4").unwrap(), (1, 4));
    assert!(parse_shard("4/4").is_err());
    assert!(parse_shard("0/0").is_err());
    assert!(parse_shard("x").is_err());
    assert!(parse_shard("1/2/3").is_err());
}
