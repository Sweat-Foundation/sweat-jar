//! Generates a miniature `interest_replay/` parquet dataset for tests.

use std::path::{Path, PathBuf};

/// Writes `<dir>/{events,accounts,account_timezones}/*.parquet` and returns `dir`.
///
/// Column names/types mirror the real `interest_replay/` export.
pub fn write_fixture_dataset(dir: &Path) -> PathBuf {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    for sub in ["events", "accounts", "account_timezones"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }

    // --- accounts: 5 accounts ---
    // 100: existed at start, timezone UTC+3
    // 200: fresh in window, timezone UTC-5
    // 300: fresh, no timezone
    // 400: fresh, single-jar cross-product restake + a future-dated increment
    // 500: fresh, multi-jar restake
    // 600: fresh per the export, but its first event is a claim, not a
    //      deposit — stands in for an FT-transfer migration (writes storage
    //      directly, emits no event) that the block-H baseline can't see.
    conn.execute_batch(&format!(
        r#"
        COPY (SELECT
                col0::BIGINT   AS backend_account_id,
                col1::VARCHAR  AS near_account_id,
                col2::BOOLEAN  AS existed_at_start,
                col3::BOOLEAN  AS created_in_window
            FROM (VALUES
                (100, 'near100', true,  false),
                (200, 'near200', false, true),
                (300, 'near300', false, true),
                (400, 'near400', false, true),
                (500, 'near500', false, true),
                (600, 'near600', false, true)
            ) t(col0, col1, col2, col3))
        TO '{d}/accounts/a.parquet' (FORMAT parquet);

        COPY (SELECT
                col0::BIGINT   AS backend_account_id,
                col1::VARCHAR  AS near_account_id,
                col2::BIGINT   AS timezone_ms,
                col3::VARCHAR  AS tz_source
            FROM (VALUES
                (100, 'near100', 10800000::BIGINT,   'user'),
                (200, 'near200', -18000000::BIGINT,  'user'),
                (300, 'near300', NULL::BIGINT,       'none'),
                (400, 'near400', NULL::BIGINT,       'none'),
                (500, 'near500', NULL::BIGINT,       'none'),
                (600, 'near600', NULL::BIGINT,       'none')
            ) t(col0, col1, col2, col3))
        TO '{d}/account_timezones/tz.parquet' (FORMAT parquet);
        "#,
        d = dir.display()
    ))
    .unwrap();

    // --- events ---
    // col6 (block_height) is arbitrary but ascending-with-time, matching the
    // real export's shape closely enough for the fallback-fetch tests, which
    // only care about the exact height of one particular row.
    conn.execute_batch(&format!(
        r#"
        COPY (SELECT
                col0::BIGINT       AS backend_account_id,
                col1::TIMESTAMP    AS block_timestamp_utc,
                col2::BIGINT       AS log_index,
                'SUCCESS_VALUE'    AS receipt_status,
                col3::VARCHAR      AS event,
                col4::VARCHAR      AS role,
                col5::VARCHAR      AS payload,
                col6::BIGINT       AS block_height
            FROM (VALUES
                (200, TIMESTAMP '2026-03-21 00:00:00', 0, 'deposit',        NULL::VARCHAR,      '["h",["steps_365d_20000_10000_tiered_v1","2000000000000000000000"]]', 190000100),
                (200, TIMESTAMP '2026-03-21 06:00:00', 0, 'record_score',   NULL::VARCHAR,      '[[9000,1774065600000]]', 190000200),
                (200, TIMESTAMP '2026-03-22 06:00:00', 0, 'apply_booster',  'applied'::VARCHAR, '{{"timestamp":"1774155600000","score":"3000"}}', 190000300),
                (200, TIMESTAMP '2026-03-25 12:00:00', 0, 'claim',          NULL::VARCHAR,      '["h",{{"items":[["steps_365d_20000_10000_tiered_v1","123"]],"timestamp":1774785600000}}]', 190000400),
                (300, TIMESTAMP '2026-03-21 00:00:00', 0, 'deposit',        NULL::VARCHAR,      '["h",["365d_12apy","1000000000000000000000"]]', 190000500),
                (300, TIMESTAMP '2026-03-30 00:00:00', 0, 'withdraw_all',   NULL::VARCHAR,      '["h",[["365d_12apy","0","1000000000000000000000"]]]', 190000600),
                (300, TIMESTAMP '2026-03-21 00:00:01', 1, 'record_score',   NULL::VARCHAR,      '[]', 190000500),
                -- 400: increment/booster timestamps ahead of their own block time (export artifact),
                -- then a single-jar restake into a *different* product.
                (400, TIMESTAMP '2026-03-21 06:00:00', 0, 'record_score',   NULL::VARCHAR,      '[[9000,1999999999999],[1000,1774054800000]]', 190000700),
                (400, TIMESTAMP '2026-03-21 07:00:00', 0, 'apply_booster',  'applied'::VARCHAR, '{{"timestamp":"1999999999999","score":"3000"}}', 190000710),
                (400, TIMESTAMP '2026-03-28 00:00:00', 0, 'restake',        NULL::VARCHAR,      '["h",{{"from":["365d_12apy"],"into":"steps_365d_20000_10000_tiered_v1","is_success":true,"restaked":"7","withdrawn":"0","timestamp":1}}]', 190000720),
                -- 500: restake consuming two jars -> restake_all sweep.
                (500, TIMESTAMP '2026-03-28 00:00:00', 0, 'restake',        NULL::VARCHAR,      '["h",{{"from":["365d_12apy","90d_3apy"],"into":"365d_12apy","is_success":true,"restaked":"9","withdrawn":"0","timestamp":1}}]', 190000800),
                -- 600: first event is a claim (no preceding deposit anywhere in
                -- the export) at block 190000900 — the fallback-fetch scenario.
                (600, TIMESTAMP '2026-03-22 00:00:00', 0, 'claim',          NULL::VARCHAR,      '["h",{{"items":[["365d_12apy","5"]],"timestamp":1774483200000}}]', 190000900),
                (600, TIMESTAMP '2026-03-29 00:00:00', 0, 'withdraw_all',   NULL::VARCHAR,      '["h",[["365d_12apy","0","10"]]]', 190001000)
            ) t(col0,col1,col2,col3,col4,col5,col6))
        TO '{d}/events/e.parquet' (FORMAT parquet);
        "#,
        d = dir.display()
    ))
    .unwrap();

    dir.to_path_buf()
}
