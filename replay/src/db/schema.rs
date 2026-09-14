//! DDL for the replay DuckDB database.

use anyhow::Context;
use duckdb::Connection;

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS events (
    backend_account_id  BIGINT  NOT NULL,
    ts_ms               BIGINT  NOT NULL,
    log_index           BIGINT  NOT NULL,
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
