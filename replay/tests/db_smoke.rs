use replay::db;

#[test]
fn open_write_then_read_roundtrips_schema() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("t.duckdb");
    {
        let conn = db::open_write(&path).unwrap();
        db::schema::init_schema(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO accounts VALUES (1, 'near1', true, 10800000);"
        ).unwrap();
    }
    let conn = db::open_read(&path).unwrap();
    let n: i64 = conn
        .query_row("SELECT count(*) FROM accounts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn results_table_upserts_by_account_id() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("r.duckdb");
    let conn = db::open_write(&path).unwrap();
    db::schema::init_schema(&conn).unwrap();

    conn.execute(
        "INSERT INTO results VALUES (1, 'near1', '100', '90', '10', 0.11, 2, 'ok', 1000)",
        [],
    )
    .unwrap();
    // Same account_id again — must overwrite in place, not duplicate.
    conn.execute(
        "INSERT INTO results VALUES (1, 'near1', '200', '90', '110', 1.22, 3, 'ok', 2000) \
         ON CONFLICT (backend_account_id) DO UPDATE SET \
            calculated_total_claim = excluded.calculated_total_claim, \
            n_claims = excluded.n_claims, \
            computed_at = excluded.computed_at",
        [],
    )
    .unwrap();

    let n: i64 = conn.query_row("SELECT count(*) FROM results", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);
    let (calc, computed_at): (String, i64) = conn
        .query_row(
            "SELECT calculated_total_claim, computed_at FROM results WHERE backend_account_id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(calc, "200");
    assert_eq!(computed_at, 2000);
}
