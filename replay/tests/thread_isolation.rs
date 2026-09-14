//! Regression guard: near-sdk's `testing_env!` persists thread-local mock
//! storage across calls on one thread. Reconciling the same DuckDB dataset with
//! 1 worker and with 3 workers must produce an identical set of output rows.

mod common;
use common::write_fixture_dataset;

use replay::db;
use replay::db::ingest::{build_db, BuildOpts};
use replay::run::{run, RunOpts};

fn build_fixture_db(dir: &std::path::Path) -> std::path::PathBuf {
    let src = dir.join("src");
    write_fixture_dataset(&src);
    let dbp = dir.join("t.duckdb");
    let mut c = db::open_write(&dbp).unwrap();
    db::schema::init_schema(&c).unwrap();
    build_db(&mut c, &BuildOpts { source_dir: &src, accounts: None, sample: None }).unwrap();
    drop(c);
    dbp
}

fn sorted_rows(csv: &str) -> Vec<String> {
    let mut lines: Vec<String> = csv.lines().skip(1).map(str::to_string).collect();
    lines.sort();
    lines
}

fn run_with(dbp: &std::path::Path, out: &std::path::Path, threads: usize, force: bool) -> Vec<String> {
    run(&RunOpts {
        db: dbp.to_path_buf(),
        out: Some(out.to_path_buf()),
        products: "tests/fixtures/products.json".into(),
        threads,
        shard: None,
        accounts: None,
        sample: None,
        tolerance: 1e-6,
        archival_rpc_url: None,
        force,
    })
    .unwrap();
    sorted_rows(&std::fs::read_to_string(out).unwrap())
}

#[test]
fn one_and_three_threads_produce_the_same_rows() {
    let d = tempfile::tempdir().unwrap();
    let dbp = build_fixture_db(d.path());

    let rows_1 = run_with(&dbp, &d.path().join("t1.csv"), 1, false);
    // Same db as the first run: force a genuine recompute rather than
    // observing the (trivially-identical) resumed no-op export.
    let rows_3 = run_with(&dbp, &d.path().join("t3.csv"), 3, true);

    assert_eq!(rows_1.len(), 5, "expected one row per fixture account");
    assert_eq!(rows_1, rows_3);
}
