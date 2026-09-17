//! DDL for the replay DuckDB database.

use anyhow::Context;
use duckdb::Connection;

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS events (
    backend_account_id  BIGINT  NOT NULL,
    ts_ms               BIGINT  NOT NULL,
    log_index           BIGINT  NOT NULL,
    -- The exact NEAR block this event executed in. Not used for ordering
    -- (ts_ms + log_index already give exact intra-account order) — only as a
    -- fallback re-fetch point: if the block-H baseline is confirmed empty but
    -- an account's first action isn't a Deposit, we know something created it
    -- invisibly (e.g. an FT-transfer migration that writes storage directly,
    -- no event emitted) and can re-fetch state at this block instead.
    block_height        BIGINT  NOT NULL,
    event               VARCHAR NOT NULL,
    role                VARCHAR,
    payload             VARCHAR NOT NULL
);
CREATE TABLE IF NOT EXISTS accounts (
    backend_account_id  BIGINT PRIMARY KEY,
    near_account_id     VARCHAR NOT NULL,
    existed_at_start    BOOLEAN NOT NULL,
    timezone_ms         BIGINT
);
CREATE TABLE IF NOT EXISTS migrations (
    -- One row per account with a JarsMerge event on the OLD (pre-v2)
    -- contract — the authoritative record of an FtMessage::Migrate that
    -- invisibly created this account on v2 sometime within the replay
    -- window (store_account_raw writes storage directly, no v2-side event).
    -- `block_height` is JarsMerge's own block, on the OLD contract's
    -- callback, which resolves strictly after v2's write already landed —
    -- so it's always safe to fetch v2 state there directly, no lock/replay
    -- ambiguity. `raw_account` is the exact borsh bytes v2's
    -- store_account_raw wrote (decoded from the export's
    -- migrated_jars_borsh_base64), when the export captured them (~98% of
    -- rows) — NULL falls back to a single archival fetch at `block_height`.
    backend_account_id  BIGINT PRIMARY KEY,
    block_height        BIGINT NOT NULL,
    raw_account         BLOB
);
CREATE TABLE IF NOT EXISTS snapshots (
    backend_account_id  BIGINT PRIMARY KEY,
    state_json          VARCHAR NOT NULL
);
CREATE TABLE IF NOT EXISTS meta (
    key    VARCHAR PRIMARY KEY,
    value  VARCHAR NOT NULL
);
CREATE TABLE IF NOT EXISTS results (
    backend_account_id      BIGINT PRIMARY KEY,
    near_account_id         VARCHAR NOT NULL,
    calculated_total_claim  VARCHAR NOT NULL,
    actual_total_claim      VARCHAR NOT NULL,
    delta                   VARCHAR NOT NULL,
    rel_delta                DOUBLE NOT NULL,
    n_claims                BIGINT NOT NULL,
    status                  VARCHAR NOT NULL,
    computed_at             BIGINT NOT NULL
);
";

pub fn init_schema(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(SCHEMA_SQL).context("init_schema")
}
