# Event-Sourced Replay Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replay the exhaustive on-chain event stream in `test_data/interest_replay/` directly, replacing the synthesized-event input layer of the `replay/` tool.

**Architecture:** Keep the `sweat_jar` `replay-engine` feature, `engine::run_timeline`, the archival baseline, the threaded driver, and the `reconciliation.csv` shape. Swap the DB backend `rusqlite` → `duckdb` (reads the parquet natively). `build-db` writes a sorted `.duckdb`; `timeline.rs` maps event rows → engine `Action`s; the engine gets three additive `Action` variants plus an upfront `set_timezone`.

**Tech Stack:** Rust, `duckdb` crate (`bundled`), `serde_json`, `clap`, the existing `sweat_jar` contract crate with `--features replay-engine`, `--profile release-replay`.

**Spec:** `docs/superpowers/specs/2026-09-10-event-sourced-replay-design.md`

## Global Constraints

- Build/run the tool with `cargo build -p replay --profile release-replay` → `target/release-replay/replay`. Never touch `.cargo/config.toml` `[profile.release]` or the root `[profile.release-replay]`.
- The golden contract test must stay exact: `cargo test -p sweat_jar --features replay-engine replay_account_history` prints `TOTAL CLAIMED: 430841686064034204316387`. Engine changes are **additive only** — existing `Action` variants (`RecordScore`, `Deposit`, `Withdraw`, `Restake`, `SetIncreasedScoreCap`, `Claim`) keep their current shape and behaviour.
- `contract/src/replay/engine.rs` has a `compile_error!` wasm guard and is host-only; keep it that way. Do not widen the `replay-engine` feature's cfg surface.
- No AI-attribution trailers beyond the ones the session reminder specifies.
- `test_data/interest_replay/` is multi-GB and gitignored — never `git add` it. Test fixtures are generated at test time, not committed as parquet.
- `cargo machete` and `cargo clippy -p replay` must stay clean.
- Commit messages end with:
  `Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>`

## Interfaces at a glance (produced across tasks)

```rust
// replay/src/db/mod.rs
pub fn open_write(path: &Path) -> anyhow::Result<duckdb::Connection>;
pub fn open_read(path: &Path) -> anyhow::Result<duckdb::Connection>;

// replay/src/payload.rs
pub enum ParsedEvent {
    RecordScore(Vec<(u16, u64)>),              // (score, ts_ms) pairs
    ApplyBooster { score: u16, timestamp_ms: u64 },
    Deposit { product_id: String, amount: u128 },
    WithdrawAll { product_ids: Vec<String> },
    Restake { into: String, from: Vec<String>, restaked: u128 },
    SetIncreasedScoreCap(bool),
    Claim { total: u128 },
}
pub fn parse_event(event: &str, role: Option<&str>, payload: &str) -> anyhow::Result<Option<ParsedEvent>>;

// replay/src/timeline.rs
pub struct UserSlice { pub backend_account_id: i64, pub near_account_id: String,
                       pub existed_at_start: bool, pub timezone_ms: Option<i64>,
                       pub onchain_claimed: u128 }
pub fn load_user(conn: &duckdb::Connection, backend_account_id: i64)
    -> anyhow::Result<(UserSlice, sweat_jar::replay::engine::Timeline)>;

// contract/src/replay/engine.rs — new Action variants
Action::ApplyBooster { score: Score, timestamp_ms: u64 }
Action::WithdrawAll { product_ids: Vec<String> }
Action::RestakeAll { product_id: String, amount: u128 }
// Baseline gains:
pub struct Baseline { pub account_id: AccountId, pub raw_account: Option<Vec<u8>>, pub timezone_ms: Option<i64> }
```

---

### Task 1: Swap the DB backend to DuckDB

**Files:**
- Modify: `replay/Cargo.toml`
- Modify: `replay/src/db/mod.rs`
- Modify: `replay/src/db/schema.rs`
- Test: `replay/tests/db_smoke.rs` (create)

**Interfaces:**
- Produces: `db::open_write(&Path) -> Result<duckdb::Connection>`, `db::open_read(&Path) -> Result<duckdb::Connection>`, `db::schema::init_schema(&duckdb::Connection) -> Result<()>`.

- [ ] **Step 1: Cargo.toml** — remove `rusqlite`, add under `[dependencies]`:
  ```toml
  duckdb = { version = "1.4", features = ["bundled"] }
  ```
  (Use the latest `1.x` the registry resolves; `cargo update -p duckdb` after.) Keep `csv` for now (removed in Task 9) — actually leave it, Task 9 handles cleanup.

- [ ] **Step 2: `db/mod.rs`** — replace the body with:
  ```rust
  //! DuckDB connection helpers for the replay database.

  pub mod ingest;
  pub mod schema;

  use std::path::Path;

  use anyhow::Context;
  use duckdb::Connection;

  /// Open (creating if needed) for building. `threads` PRAGMA left at the
  /// DuckDB default (all cores) — `build-db` is a one-shot bulk job.
  pub fn open_write(path: &Path) -> anyhow::Result<Connection> {
      Connection::open(path).with_context(|| format!("open_write: {}", path.display()))
  }

  /// Open an existing database read-only.
  pub fn open_read(path: &Path) -> anyhow::Result<Connection> {
      let cfg = duckdb::Config::default().access_mode(duckdb::AccessMode::ReadOnly)?;
      Connection::open_with_flags(path, cfg)
          .with_context(|| format!("open_read: {}", path.display()))
  }
  ```

- [ ] **Step 3: `db/schema.rs`** — replace `SCHEMA_SQL` / `INDEX_SQL` / functions with:
  ```rust
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
  ";

  pub fn init_schema(conn: &Connection) -> anyhow::Result<()> {
      conn.execute_batch(SCHEMA_SQL).context("init_schema")
  }
  ```
  Delete `create_indexes` (DuckDB uses zonemaps on the sorted `events` table; no
  secondary index needed). Grep for `create_indexes` callers and remove them
  (there is one in `db/ingest.rs`, rewritten in Task 3).

- [ ] **Step 4: make it compile** — `db/ingest.rs`, `timeline.rs`, `reconcile.rs`, `snapshot.rs`, `explain.rs`, `run.rs`, `tests/*` still reference `rusqlite`. For THIS task, get `replay` to build by:
  - `db/ingest.rs`: replace its whole body with a stub `pub struct BuildOpts<'a> { pub source_dir: &'a std::path::Path, pub accounts: Option<&'a std::collections::HashSet<i64>>, pub sample: Option<usize> } pub fn build_db(_c: &mut duckdb::Connection, _o: &BuildOpts) -> anyhow::Result<Vec<(String, i64)>> { anyhow::bail!("build_db: implemented in Task 3") }` (Task 3 fills it).
  - `timeline.rs`, `reconcile.rs`, `snapshot.rs`, `explain.rs`: change `rusqlite::Connection` → `duckdb::Connection` in signatures; where `.query_map` / `.prepare` APIs differ, use duckdb's (`conn.prepare(sql)?; let rows = stmt.query_map(params![...], |r| ...)?;` — duckdb mirrors rusqlite closely, `duckdb::params!`). Make the minimal edits to compile; Tasks 5–7 rewrite the bodies.
  - Comment out / `#[ignore]` the failing integration tests that depend on CSV fixtures (`tests/timeline.rs`, `tests/reconcile.rs`, `tests/run_e2e.rs`, `tests/build_db.rs`, `tests/thread_isolation.rs`) with a `// TODO(event-sourced): rewritten in Task N` note. Task 8 restores them.

- [ ] **Step 5: `tests/db_smoke.rs`** — new:
  ```rust
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
  ```

- [ ] **Step 6: Run** `cargo build -p replay --profile release-replay` and `cargo test -p replay --profile release-replay --test db_smoke`. Expected: build OK, smoke passes.

- [ ] **Step 7: Commit** — `git add replay/Cargo.toml replay/Cargo.lock replay/src/db replay/src/timeline.rs replay/src/reconcile.rs replay/src/snapshot.rs replay/src/explain.rs replay/src/run.rs replay/tests && git commit -m "refactor(replay): swap rusqlite for duckdb backend"`

---

### Task 2: Payload parsing module

**Files:**
- Create: `replay/src/payload.rs`
- Modify: `replay/src/lib.rs` (add `pub mod payload;`)
- Modify: `replay/src/parse.rs` (nothing to change; `yocto_str_to_u128` is reused)

**Interfaces:**
- Consumes: `crate::parse::yocto_str_to_u128`.
- Produces: `payload::ParsedEvent` (enum above), `payload::parse_event(event: &str, role: Option<&str>, payload: &str) -> anyhow::Result<Option<ParsedEvent>>`.

- [ ] **Step 1: Write the failing test** — `replay/src/payload.rs` `#[cfg(test)] mod tests`:
  ```rust
  use super::*;

  #[test]
  fn record_score_pairs() {
      let p = parse_event("record_score", None, "[[1345,1787183770765],[2029,1787172973179]]")
          .unwrap()
          .unwrap();
      assert!(matches!(p, ParsedEvent::RecordScore(ref v)
          if v == &vec![(1345u16, 1787183770765u64), (2029, 1787172973179)]));
  }

  #[test]
  fn record_score_empty_is_none() {
      assert!(parse_event("record_score", None, "[]").unwrap().is_none());
  }

  #[test]
  fn apply_booster_applied() {
      let p = parse_event("apply_booster", Some("applied"),
          r#"{"timestamp":"1787025600000","score":"3000"}"#).unwrap().unwrap();
      assert!(matches!(p, ParsedEvent::ApplyBooster { score: 3000, timestamp_ms: 1787025600000 }));
  }

  #[test]
  fn apply_booster_rejected_is_none() {
      assert!(parse_event("apply_booster", Some("rejected"),
          r#"{"timestamp":"1","score":"1"}"#).unwrap().is_none());
  }

  #[test]
  fn deposit_pair() {
      let p = parse_event("deposit", None,
          r#"["hash",["steps_365d_20000_10000_tiered_v1","1000000000000000000"]]"#)
          .unwrap().unwrap();
      assert!(matches!(p, ParsedEvent::Deposit { ref product_id, amount: 1_000_000_000_000_000_000 }
          if product_id == "steps_365d_20000_10000_tiered_v1"));
  }

  #[test]
  fn withdraw_all_list() {
      let p = parse_event("withdraw_all", None,
          r#"["hash",[["365d_12apy","0","50000000000000000000"],["90d_3apy","0","1"]]]"#)
          .unwrap().unwrap();
      assert!(matches!(p, ParsedEvent::WithdrawAll { ref product_ids }
          if product_ids == &vec!["365d_12apy".to_string(), "90d_3apy".to_string()]));
  }

  #[test]
  fn restake_success() {
      let p = parse_event("restake", None,
          r#"["hash",{"from":["a","b"],"into":"365d_12apy","is_success":true,"restaked":"1248350000000000000000","withdrawn":"0","timestamp":1}"#)
          .unwrap().unwrap();
      assert!(matches!(p, ParsedEvent::Restake { ref into, ref from, restaked: 1_248_350_000_000_000_000_000 }
          if into == "365d_12apy" && from == &vec!["a".to_string(), "b".to_string()]));
  }

  #[test]
  fn restake_failure_is_none() {
      assert!(parse_event("restake", None,
          r#"["hash",{"from":["a"],"into":"a","is_success":false,"restaked":"0","withdrawn":"0","timestamp":1}"#)
          .unwrap().is_none());
  }

  #[test]
  fn set_feature_enabled_bool() {
      let p = parse_event("set_feature_enabled", None,
          r#"["hash","increased_score_cap",true]"#).unwrap().unwrap();
      assert!(matches!(p, ParsedEvent::SetIncreasedScoreCap(true)));
  }

  #[test]
  fn claim_items_sum() {
      let p = parse_event("claim", None,
          r#"["hash",{"items":[["p1","10"],["p2","5"]],"timestamp":1}]"#).unwrap().unwrap();
      assert!(matches!(p, ParsedEvent::Claim { total: 15 }));
  }

  #[test]
  fn unknown_event_errors() {
      assert!(parse_event("frobnicate", None, "{}").is_err());
  }
  ```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p replay --profile release-replay --lib payload` → FAIL (module empty).

- [ ] **Step 3: Implement `replay/src/payload.rs`:**
  ```rust
  //! Parse the JSON `payload` string of an on-chain event row into a typed
  //! [`ParsedEvent`]. Returns `Ok(None)` for rows that map to no engine action.

  use anyhow::{bail, Context, Result};
  use serde_json::Value;

  use crate::parse::yocto_str_to_u128;

  #[derive(Debug, Clone, PartialEq)]
  pub enum ParsedEvent {
      RecordScore(Vec<(u16, u64)>),
      ApplyBooster { score: u16, timestamp_ms: u64 },
      Deposit { product_id: String, amount: u128 },
      WithdrawAll { product_ids: Vec<String> },
      Restake { into: String, from: Vec<String>, restaked: u128 },
      SetIncreasedScoreCap(bool),
      Claim { total: u128 },
  }

  /// `role` is the event row's `role` column (only meaningful for `apply_booster`).
  pub fn parse_event(event: &str, role: Option<&str>, payload: &str) -> Result<Option<ParsedEvent>> {
      let v: Value = serde_json::from_str(payload)
          .with_context(|| format!("payload not JSON for {event}: {payload:?}"))?;
      match event {
          "record_score" => {
              let pairs = v.as_array().context("record_score payload not an array")?;
              let mut out = Vec::with_capacity(pairs.len());
              for p in pairs {
                  let a = p.as_array().context("record_score pair not an array")?;
                  let score = u16::try_from(a[0].as_u64().context("score not u64")?.min(u16::MAX.into()))
                      .unwrap_or(u16::MAX);
                  let ts = a[1].as_u64().context("score ts not u64")?;
                  out.push((score, ts));
              }
              Ok((!out.is_empty()).then_some(ParsedEvent::RecordScore(out)))
          }
          "apply_booster" => {
              if role != Some("applied") {
                  return Ok(None);
              }
              let score = str_num_u16(&v["score"]).context("booster score")?;
              let timestamp_ms = str_num_u64(&v["timestamp"]).context("booster timestamp")?;
              Ok(Some(ParsedEvent::ApplyBooster { score, timestamp_ms }))
          }
          "deposit" => {
              let inner = &v[1];
              Ok(Some(ParsedEvent::Deposit {
                  product_id: inner[0].as_str().context("deposit product_id")?.to_string(),
                  amount: yocto_str_to_u128(inner[1].as_str().context("deposit amount")?)?,
              }))
          }
          "withdraw_all" => {
              let rows = v[1].as_array().context("withdraw_all list")?;
              let product_ids = rows
                  .iter()
                  .map(|r| r[0].as_str().map(str::to_string).context("withdraw_all product_id"))
                  .collect::<Result<Vec<_>>>()?;
              Ok(Some(ParsedEvent::WithdrawAll { product_ids }))
          }
          "restake" => {
              let d = &v[1];
              if !d["is_success"].as_bool().unwrap_or(false) {
                  return Ok(None);
              }
              let from = d["from"]
                  .as_array()
                  .context("restake from")?
                  .iter()
                  .map(|s| s.as_str().map(str::to_string).context("restake from item"))
                  .collect::<Result<Vec<_>>>()?;
              Ok(Some(ParsedEvent::Restake {
                  into: d["into"].as_str().context("restake into")?.to_string(),
                  from,
                  restaked: yocto_str_to_u128(d["restaked"].as_str().context("restake restaked")?)?,
              }))
          }
          "set_feature_enabled" => {
              // ["hash", "increased_score_cap", bool]
              Ok(Some(ParsedEvent::SetIncreasedScoreCap(
                  v[2].as_bool().context("set_feature_enabled value")?,
              )))
          }
          "claim" => {
              let items = v[1]["items"].as_array().context("claim items")?;
              let mut total = 0u128;
              for it in items {
                  total = total
                      .checked_add(yocto_str_to_u128(it[1].as_str().context("claim item amount")?)?)
                      .context("claim total overflow")?;
              }
              Ok(Some(ParsedEvent::Claim { total }))
          }
          other => bail!("unknown event type {other:?}"),
      }
  }

  fn str_num_u16(v: &Value) -> Result<u16> {
      match v {
          Value::String(s) => Ok(s.parse()?),
          Value::Number(n) => u16::try_from(n.as_u64().context("not u64")?).context("u16 overflow"),
          _ => bail!("expected string or number, got {v:?}"),
      }
  }

  fn str_num_u64(v: &Value) -> Result<u64> {
      match v {
          Value::String(s) => Ok(s.parse()?),
          Value::Number(n) => n.as_u64().context("not u64"),
          _ => bail!("expected string or number, got {v:?}"),
      }
  }
  ```

- [ ] **Step 4: `lib.rs`** — add `pub mod payload;` (alphabetical: after `parse`).

- [ ] **Step 5: Run** `cargo test -p replay --profile release-replay --lib payload` → PASS (12 tests).

- [ ] **Step 6: Commit** — `git add replay/src/payload.rs replay/src/lib.rs && git commit -m "feat(replay): typed parser for on-chain event payloads"`

---

### Task 3: `build-db` — parquet → DuckDB

**Files:**
- Modify: `replay/src/db/ingest.rs` (replace the Task 1 stub)
- Modify: `replay/src/cli.rs` (BuildDb args)
- Modify: `replay/src/main.rs` (BuildDb arm)
- Modify: `replay/src/parse.rs` (expose `H_MS` / `T_END_MS` — already `pub`)
- Test: `replay/tests/fixtures.rs` (create — shared fixture generator), `replay/tests/build_db.rs` (rewrite)

**Interfaces:**
- Consumes: `db::open_write`, `db::schema::init_schema`.
- Produces: `ingest::BuildOpts<'a> { source_dir: &'a Path, accounts: Option<&'a HashSet<i64>>, sample: Option<usize> }`, `ingest::build_db(&mut duckdb::Connection, &BuildOpts) -> Result<Vec<(String, i64)>>` (returns `(table_name, row_count)` for CLI printout), `ingest::read_accounts(&Path) -> Result<Vec<i64>>` (keep the existing helper — reads a newline / comma list of ints).

- [ ] **Step 1: `tests/fixtures.rs`** — shared helper that writes a tiny parquet dataset:
  ```rust
  //! Generates a miniature `interest_replay/` parquet dataset for tests.
  use std::path::{Path, PathBuf};

  /// Writes `<dir>/{events,accounts,account_timezones}/*.parquet` and returns `dir`.
  pub fn write_fixture_dataset(dir: &Path) -> PathBuf {
      let conn = duckdb::Connection::open_in_memory().unwrap();
      for sub in ["events", "accounts", "account_timezones"] {
          std::fs::create_dir_all(dir.join(sub)).unwrap();
      }
      // --- accounts: 3 accounts ---
      // 100: existed at start, has a score jar in the baseline (archival test uses
      //      a stub snapshot), timezone UTC+3
      // 200: fresh in window, score jar + booster + claim, timezone UTC-5
      // 300: fresh, one fixed deposit + withdraw_all, no timezone
      conn.execute_batch(&format!(r#"
          COPY (SELECT * FROM (VALUES
              (100, 'near100', true,  10800000),
              (200, 'near200', false, -18000000),
              (300, 'near300', false, NULL)
          ) t(backend_account_id, near_account_id, existed_at_start, timezone_ms))
          TO '{d}/accounts/a.parquet' (FORMAT parquet);

          COPY (SELECT * FROM (VALUES
              (100, 'near100', 10800000),
              (200, 'near200', -18000000),
              (300, 'near300', NULL)
          ) t(backend_account_id, near_account_id, timezone_ms))
          TO '{d}/account_timezones/tz.parquet' (FORMAT parquet);
      "#, d = dir.display())).unwrap();
      // --- events (timestamps just after H = 2026-03-20 14:41:50.156Z) ---
      conn.execute_batch(&format!(r#"
          COPY (SELECT
                  col0::BIGINT       AS backend_account_id,
                  col1::TIMESTAMP    AS block_timestamp_utc,
                  col2::BIGINT       AS log_index,
                  'SUCCESS_VALUE'    AS receipt_status,
                  col3::VARCHAR      AS event,
                  col4               AS role,
                  col5::VARCHAR      AS payload
              FROM (VALUES
                  (200, TIMESTAMP '2026-03-21 00:00:00', 0, 'deposit',        NULL,       '["h",["steps_365d_20000_10000_tiered_v1","2000000000000000000000"]]'),
                  (200, TIMESTAMP '2026-03-21 06:00:00', 0, 'record_score',   NULL,       '[[9000,1774419000000]]'),
                  (200, TIMESTAMP '2026-03-22 06:00:00', 0, 'apply_booster',  'applied',  '{{"timestamp":"1774497600000","score":"3000"}}'),
                  (200, TIMESTAMP '2026-03-25 12:00:00', 0, 'claim',          NULL,       '["h",{{"items":[["steps_365d_20000_10000_tiered_v1","123"]],"timestamp":1774785600000}}]'),
                  (300, TIMESTAMP '2026-03-21 00:00:00', 0, 'deposit',        NULL,       '["h",["365d_12apy","1000000000000000000000"]]'),
                  (300, TIMESTAMP '2026-03-30 00:00:00', 0, 'withdraw_all',   NULL,       '["h",[["365d_12apy","0","1000000000000000000000"]]]'),
                  (300, TIMESTAMP '2026-03-21 00:00:01', 1, 'record_score',   NULL,       '[]')
              ) t(col0,col1,col2,col3,col4,col5))
          TO '{d}/events/e.parquet' (FORMAT parquet);
      "#, d = dir.display())).unwrap();
      dir.to_path_buf()
  }
  ```
  (If DuckDB rejects `NULL` typing in a `VALUES` list, wrap with `col3::BIGINT`
  and cast `NULL::BIGINT` explicitly — adjust until `write_fixture_dataset`
  runs.)

- [ ] **Step 2: Write the failing test** — `replay/tests/build_db.rs` (full rewrite):
  ```rust
  mod fixtures;
  use fixtures::write_fixture_dataset;
  use replay::db;
  use replay::db::ingest::{build_db, BuildOpts};

  fn built(dir: &std::path::Path) -> duckdb::Connection {
      let src = dir.join("src");
      write_fixture_dataset(&src);
      let dbp = dir.join("out.duckdb");
      let mut conn = db::open_write(&dbp).unwrap();
      db::schema::init_schema(&conn).unwrap();
      build_db(&mut conn, &BuildOpts { source_dir: &src, accounts: None, sample: None }).unwrap();
      conn
  }

  #[test]
  fn events_are_success_only_and_sorted() {
      let d = tempfile::tempdir().unwrap();
      let conn = built(d.path());
      let n: i64 = conn.query_row("SELECT count(*) FROM events", [], |r| r.get(0)).unwrap();
      assert_eq!(n, 7); // all fixture rows are SUCCESS_VALUE
      let ordered: bool = conn.query_row(
          "SELECT bool_and(ok) FROM (SELECT (backend_account_id, ts_ms, log_index) >= \
           lag((backend_account_id, ts_ms, log_index)) OVER () AS ok FROM events)",
          [], |r| r.get::<_, Option<bool>>(0).map(|o| o.unwrap_or(true))).unwrap();
      assert!(ordered);
  }

  #[test]
  fn accounts_join_carries_timezone_and_existed_flag() {
      let d = tempfile::tempdir().unwrap();
      let conn = built(d.path());
      let (existed, tz): (bool, Option<i64>) = conn.query_row(
          "SELECT existed_at_start, timezone_ms FROM accounts WHERE backend_account_id = 100",
          [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
      assert!(existed);
      assert_eq!(tz, Some(10_800_000));
      let tz300: Option<i64> = conn.query_row(
          "SELECT timezone_ms FROM accounts WHERE backend_account_id = 300", [], |r| r.get(0)).unwrap();
      assert_eq!(tz300, None);
  }

  #[test]
  fn sample_limits_accounts_and_their_events() {
      let d = tempfile::tempdir().unwrap();
      let src = d.path().join("src");
      write_fixture_dataset(&src);
      let dbp = d.path().join("s.duckdb");
      let mut conn = db::open_write(&dbp).unwrap();
      db::schema::init_schema(&conn).unwrap();
      build_db(&mut conn, &BuildOpts { source_dir: &src, accounts: None, sample: Some(1) }).unwrap();
      let a: i64 = conn.query_row("SELECT count(*) FROM accounts", [], |r| r.get(0)).unwrap();
      assert_eq!(a, 1);
      let stray: i64 = conn.query_row(
          "SELECT count(*) FROM events WHERE backend_account_id NOT IN (SELECT backend_account_id FROM accounts)",
          [], |r| r.get(0)).unwrap();
      assert_eq!(stray, 0);
  }
  ```

- [ ] **Step 3: Run to verify it fails** — `cargo test -p replay --profile release-replay --test build_db` → FAIL (`build_db` bails).

- [ ] **Step 4: Implement `replay/src/db/ingest.rs`:**
  ```rust
  //! Build the replay DuckDB from an `interest_replay/` parquet export.

  use std::collections::HashSet;
  use std::path::Path;

  use anyhow::{Context, Result};
  use duckdb::Connection;

  use crate::parse::{H_MS, T_END_MS};

  pub struct BuildOpts<'a> {
      pub source_dir: &'a Path,
      pub accounts: Option<&'a HashSet<i64>>,
      pub sample: Option<usize>,
  }

  /// Reads a newline/comma-separated list of integer account ids.
  pub fn read_accounts(path: &Path) -> Result<Vec<i64>> {
      let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
      text.split(|c: char| c.is_whitespace() || c == ',')
          .filter(|s| !s.is_empty())
          .map(|s| s.parse::<i64>().with_context(|| format!("bad account id {s:?}")))
          .collect()
  }

  fn glob(dir: &Path, sub: &str) -> String {
      // DuckDB read_parquet glob
      format!("{}/{sub}/*.parquet", dir.display())
  }

  pub fn build_db(conn: &mut Connection, opts: &BuildOpts) -> Result<Vec<(String, i64)>> {
      let acc_glob = glob(opts.source_dir, "accounts");
      let tz_glob = glob(opts.source_dir, "account_timezones");
      let ev_glob = glob(opts.source_dir, "events");

      // 1. accounts (+ timezone join), optionally filtered/sampled
      let mut where_acc = String::new();
      if let Some(set) = opts.accounts {
          let list = set.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
          where_acc = format!("WHERE a.backend_account_id IN ({list})");
      }
      let limit = opts.sample.map(|n| format!("LIMIT {n}")).unwrap_or_default();
      conn.execute_batch(&format!(
          "INSERT INTO accounts
           SELECT a.backend_account_id, a.near_account_id, a.existed_at_start, t.timezone_ms
           FROM read_parquet('{acc_glob}') a
           LEFT JOIN read_parquet('{tz_glob}') t USING (backend_account_id)
           {where_acc}
           ORDER BY a.backend_account_id
           {limit};"
      )).context("insert accounts")?;

      // 2. events for those accounts only, success rows, sorted
      conn.execute_batch(&format!(
          "INSERT INTO events
           SELECT e.backend_account_id,
                  epoch_ms(e.block_timestamp_utc) AS ts_ms,
                  e.log_index,
                  e.event,
                  e.role,
                  e.payload
           FROM read_parquet('{ev_glob}') e
           SEMI JOIN accounts USING (backend_account_id)
           WHERE e.receipt_status = 'SUCCESS_VALUE'
           ORDER BY e.backend_account_id, ts_ms, e.log_index;"
      )).context("insert events")?;

      // 3. meta
      let put = |k: &str, v: String| -> Result<()> {
          conn.execute("INSERT OR REPLACE INTO meta VALUES (?, ?)", duckdb::params![k, v])
              .map(|_| ()).context("meta")
      };
      put("source_dir", opts.source_dir.display().to_string())?;
      put("h_ms", H_MS.to_string())?;
      put("t_end_ms", T_END_MS.to_string())?;

      let count = |t: &str| -> Result<i64> {
          conn.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0)).context("count")
      };
      let counts = vec![
          ("accounts".to_string(), count("accounts")?),
          ("events".to_string(), count("events")?),
      ];
      for (t, n) in &counts {
          put(&format!("{t}_rows"), n.to_string())?;
      }
      Ok(counts)
  }
  ```
  Notes for the implementer: DuckDB supports `SEMI JOIN` and `epoch_ms()`. If
  `INSERT OR REPLACE` is rejected, use `INSERT INTO meta VALUES (?,?) ON CONFLICT DO UPDATE SET value = excluded.value`.
  Verify `read_parquet` glob works with an absolute path containing spaces —
  `source_dir` in tests is a tempdir (no spaces); the real path
  `test_data/interest_replay` has none.

- [ ] **Step 5: `cli.rs`** — change the `BuildDb` variant to:
  ```rust
  BuildDb {
      #[arg(long)]
      db: PathBuf,
      #[arg(long, default_value = "test_data/interest_replay")]
      source: PathBuf,
      #[arg(long)]
      accounts: Option<PathBuf>,
      #[arg(long)]
      sample: Option<usize>,
  },
  ```

- [ ] **Step 6: `main.rs`** — the `BuildDb` arm:
  ```rust
  cli::Cmd::BuildDb { db: db_path, source, accounts, sample } => {
      let accounts: Option<std::collections::HashSet<i64>> = match accounts {
          Some(p) => Some(db::ingest::read_accounts(&p)?.into_iter().collect()),
          None => None,
      };
      let mut conn = db::open_write(&db_path)?;
      db::schema::init_schema(&conn)?;
      let counts = db::ingest::build_db(&mut conn, &db::ingest::BuildOpts {
          source_dir: &source,
          accounts: accounts.as_ref(),
          sample,
      })?;
      for (t, n) in counts { println!("{t}: {n}"); }
      Ok(())
  }
  ```

- [ ] **Step 7: Run** `cargo test -p replay --profile release-replay --test build_db` → PASS (3 tests).

- [ ] **Step 8: Commit** — `git add replay/src/db/ingest.rs replay/src/cli.rs replay/src/main.rs replay/tests/build_db.rs replay/tests/fixtures.rs && git commit -m "feat(replay): build-db reads interest_replay parquet into duckdb"`

---

### Task 4: Engine — new Actions + upfront timezone

**Files:**
- Modify: `contract/src/replay/engine.rs`
- Test: `contract/src/replay/engine.rs` `#[cfg(test)]` (add), and the golden `cargo test -p sweat_jar --features replay-engine replay_account_history` must not change.

**Interfaces:**
- Produces: `Action::ApplyBooster { score: Score, timestamp_ms: u64 }`, `Action::WithdrawAll { product_ids: Vec<String> }`, `Action::RestakeAll { product_id: String, amount: u128 }`, `Baseline.timezone_ms: Option<i64>`.
- Consumes (from `sweat_jar_model::api::AccountApi`): `apply_booster`, `set_timezone`; (`WithdrawApi`): `withdraw_all`; (`RestakeApi`): `restake_all`.

- [ ] **Step 1: Add imports** — in `engine.rs` top `use` block: extend the `sweat_jar_model::api` import to `{AccountApi, ClaimApi, RestakeApi, WithdrawApi}` (already present) and add `near_sdk::json_types::I64` to the `near_sdk` import. `AccountApi` already covers `apply_booster` + `set_timezone`.

- [ ] **Step 2: Extend `Action`** — add three variants after `Claim`:
  ```rust
  /// `apply_booster([account], score, UTC(timestamp_ms))` — the oracle booster path.
  ApplyBooster { score: Score, timestamp_ms: u64 },
  /// `withdraw_all(Some(product_ids))` — matured balance of the named jars.
  WithdrawAll { product_ids: Vec<String> },
  /// `restake_all(ticket(into=product_id), None, Some(amount))`.
  RestakeAll { product_id: String, amount: u128 },
  ```
  Extend `Action::rank`: `ApplyBooster` → 0 (score-like, before claims); `WithdrawAll | RestakeAll` → 1 (state change).

- [ ] **Step 3: Extend `Baseline`:**
  ```rust
  pub struct Baseline {
      pub account_id: AccountId,
      pub raw_account: Option<Vec<u8>>,
      /// Authoritative account timezone (ms offset). `None` or `i64::MIN` -> not set.
      pub timezone_ms: Option<i64>,
  }
  ```

- [ ] **Step 4: `run_timeline` — set timezone before the loop.** Inside the `catch_unwind` closure, right after the `if let Some(raw) = &baseline.raw_account { store_account_raw(...) }` block and the `context.set_block_timestamp_in_ms(window_start_ms);` line, add:
  ```rust
  if let Some(tz) = baseline.timezone_ms.filter(|t| *t != i64::MIN) {
      context.switch_account_to_operator();
      context.contract().set_timezone(account_id.clone(), I64(tz));
  }
  ```

- [ ] **Step 5: Handle the new actions** in the `match event.action` block:
  ```rust
  Action::ApplyBooster { score, timestamp_ms } => {
      context.switch_account_to_operator();
      context
          .contract()
          .apply_booster(vec![account_id.clone()], score, UTC(timestamp_ms));
  }
  Action::WithdrawAll { product_ids } => {
      context.switch_account(&account_id);
      let set: std::collections::HashSet<String> = product_ids.into_iter().collect();
      let _ = context.contract().withdraw_all(Some(set));
  }
  Action::RestakeAll { product_id, amount } => {
      context.switch_account(&account_id);
      let ticket = DepositTicket {
          product_id: product_id.clone(),
          valid_until: 0.into(),
          timezone: Some(Timezone::hour_shift(0)),
      };
      let _ = context.contract().restake_all(ticket, None, Some(amount.into()));
  }
  ```

- [ ] **Step 6: Fix the two existing `Baseline` construction sites** — `contract/src/replay/mod.rs` (golden) and `replay/src/reconcile.rs`. For `mod.rs`, add `timezone_ms: None` (the golden fixture path keeps using the deposit-ticket timezone; behaviour unchanged). `reconcile.rs` is rewired in Task 6 — for now add `timezone_ms: None` to keep it compiling.

- [ ] **Step 7: Add an engine unit test** — `contract/src/replay/engine.rs`:
  ```rust
  #[cfg(test)]
  mod engine_tests {
      use super::*;

      #[test]
      fn new_action_variants_construct() {
          let _ = Action::ApplyBooster { score: 3000, timestamp_ms: 1 };
          let _ = Action::WithdrawAll { product_ids: vec!["p".into()] };
          let _ = Action::RestakeAll { product_id: "p".into(), amount: 1 };
          assert_eq!(Action::ApplyBooster { score: 0, timestamp_ms: 0 }.rank(), 0);
          assert_eq!(Action::WithdrawAll { product_ids: vec![] }.rank(), 1);
      }
  }
  ```

- [ ] **Step 8: Run**
  - `cargo test -p sweat_jar --features replay-engine replay_account_history -- --nocapture 2>&1 | grep "TOTAL CLAIMED"` → must print `TOTAL CLAIMED: 430841686064034204316387`.
  - `cargo build -p replay --profile release-replay` → OK.
  - `cargo test -p sweat_jar --features replay-engine engine_tests` → PASS.

- [ ] **Step 9: Commit** — `git add contract/src/replay/engine.rs contract/src/replay/mod.rs replay/src/reconcile.rs && git commit -m "feat(replay-engine): ApplyBooster/WithdrawAll/RestakeAll actions + upfront set_timezone"`

---

### Task 5: `timeline.rs` — synthesize from the events table

**Files:**
- Rewrite: `replay/src/timeline.rs`
- Test: `replay/tests/timeline.rs` (rewrite)

**Interfaces:**
- Consumes: `crate::payload::{parse_event, ParsedEvent}`, `db` connection, `engine::{Action, Event, Timeline}`.
- Produces: `timeline::UserSlice { backend_account_id: i64, near_account_id: String, existed_at_start: bool, timezone_ms: Option<i64>, onchain_claimed: u128 }`, `timeline::load_user(&duckdb::Connection, i64) -> Result<(UserSlice, Timeline)>`.

- [ ] **Step 1: Write the failing test** — `replay/tests/timeline.rs` (full rewrite):
  ```rust
  mod fixtures;
  use fixtures::write_fixture_dataset;
  use replay::db;
  use replay::db::ingest::{build_db, BuildOpts};
  use replay::timeline::load_user;
  use sweat_jar::replay::engine::Action;

  fn conn(dir: &std::path::Path) -> duckdb::Connection {
      let src = dir.join("src");
      write_fixture_dataset(&src);
      let dbp = dir.join("t.duckdb");
      let mut c = db::open_write(&dbp).unwrap();
      db::schema::init_schema(&c).unwrap();
      build_db(&mut c, &BuildOpts { source_dir: &src, accounts: None, sample: None }).unwrap();
      c
  }

  #[test]
  fn account_200_maps_all_event_kinds() {
      let d = tempfile::tempdir().unwrap();
      let c = conn(d.path());
      let (slice, tl) = load_user(&c, 200).unwrap();
      assert_eq!(slice.near_account_id, "near200");
      assert!(!slice.existed_at_start);
      assert_eq!(slice.timezone_ms, Some(-18_000_000));
      assert_eq!(slice.onchain_claimed, 123);

      let kinds: Vec<&str> = tl.events.iter().map(|e| match &e.action {
          Action::Deposit { .. } => "deposit",
          Action::RecordScore(_) => "score",
          Action::ApplyBooster { .. } => "booster",
          Action::Claim => "claim",
          _ => "other",
      }).collect();
      assert_eq!(kinds, vec!["deposit", "score", "booster", "claim"]);
  }

  #[test]
  fn account_300_withdraw_all_and_empty_score_skipped() {
      let d = tempfile::tempdir().unwrap();
      let c = conn(d.path());
      let (_slice, tl) = load_user(&c, 300).unwrap();
      // record_score '[]' produces no event; deposit + withdraw_all remain.
      assert_eq!(tl.events.len(), 2);
      assert!(matches!(tl.events[1].action, Action::WithdrawAll { ref product_ids }
          if product_ids == &vec!["365d_12apy".to_string()]));
  }

  #[test]
  fn unknown_account_is_err() {
      let d = tempfile::tempdir().unwrap();
      let c = conn(d.path());
      assert!(load_user(&c, 999).is_err());
  }
  ```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p replay --profile release-replay --test timeline` → FAIL.

- [ ] **Step 3: Rewrite `replay/src/timeline.rs`:**
  ```rust
  //! Per-account event slice -> engine [`Timeline`].

  use anyhow::{bail, Context, Result};
  use duckdb::Connection;
  use sweat_jar::replay::engine::{Action, Event, Timeline};

  use crate::payload::{parse_event, ParsedEvent};

  pub struct UserSlice {
      pub backend_account_id: i64,
      pub near_account_id: String,
      pub existed_at_start: bool,
      pub timezone_ms: Option<i64>,
      /// Sum of every `claim` event's payload `items`.
      pub onchain_claimed: u128,
  }

  pub fn load_user(conn: &Connection, backend_account_id: i64) -> Result<(UserSlice, Timeline)> {
      let (near_account_id, existed_at_start, timezone_ms): (String, bool, Option<i64>) = conn
          .query_row(
              "SELECT near_account_id, existed_at_start, timezone_ms FROM accounts WHERE backend_account_id = ?",
              duckdb::params![backend_account_id],
              |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
          )
          .with_context(|| format!("account {backend_account_id} not in accounts table"))?;

      let mut stmt = conn.prepare(
          "SELECT ts_ms, log_index, event, role, payload FROM events \
           WHERE backend_account_id = ? ORDER BY ts_ms, log_index",
      )?;
      let rows = stmt
          .query_map(duckdb::params![backend_account_id], |r| {
              Ok((
                  r.get::<_, i64>(0)? as u64,
                  r.get::<_, i64>(1)? as u64,
                  r.get::<_, String>(2)?,
                  r.get::<_, Option<String>>(3)?,
                  r.get::<_, String>(4)?,
              ))
          })?
          .collect::<duckdb::Result<Vec<_>>>()?;

      let mut events = Vec::new();
      let mut onchain_claimed = 0u128;

      for (ts_ms, log_index, event, role, payload) in rows {
          let Some(parsed) = parse_event(&event, role.as_deref(), &payload)
              .with_context(|| format!("account {backend_account_id} ts {ts_ms}"))?
          else {
              continue;
          };
          let action = match parsed {
              ParsedEvent::RecordScore(pairs) => Action::RecordScore(pairs),
              ParsedEvent::ApplyBooster { score, timestamp_ms } => {
                  Action::ApplyBooster { score, timestamp_ms }
              }
              ParsedEvent::Deposit { product_id, amount } => Action::Deposit { product_id, amount },
              ParsedEvent::WithdrawAll { product_ids } => Action::WithdrawAll { product_ids },
              ParsedEvent::Restake { into, from, restaked } => {
                  if from == [into.clone()] {
                      Action::Restake { product_id: into, amount: restaked }
                  } else {
                      Action::RestakeAll { product_id: into, amount: restaked }
                  }
              }
              ParsedEvent::SetIncreasedScoreCap(v) => Action::SetIncreasedScoreCap(v),
              ParsedEvent::Claim { total } => {
                  onchain_claimed = onchain_claimed
                      .checked_add(total)
                      .context("onchain_claimed overflow")?;
                  Action::Claim
              }
          };
          events.push(Event { ts_ms, seq: log_index, action });
      }

      if events.is_empty() {
          // Accounts with zero replayable events still reconcile (0 == 0); keep going.
      }
      let _ = bail; // silence unused import if no bail path remains

      Ok((
          UserSlice { backend_account_id, near_account_id, existed_at_start, timezone_ms, onchain_claimed },
          Timeline { events }.sorted(),
      ))
  }
  ```
  (Drop the `let _ = bail;` line and the `bail` import if unused — the
  implementer should keep only what compiles cleanly.)

- [ ] **Step 4: Run** `cargo test -p replay --profile release-replay --test timeline` → PASS (3 tests).

- [ ] **Step 5: Commit** — `git add replay/src/timeline.rs replay/tests/timeline.rs && git commit -m "feat(replay): synthesize timelines from the on-chain events table"`

---

### Task 6: `reconcile.rs` + `run.rs` + `snapshot.rs` — wire duckdb + timezone

**Files:**
- Modify: `replay/src/reconcile.rs`
- Modify: `replay/src/run.rs`
- Modify: `replay/src/snapshot.rs`
- Test: `replay/tests/reconcile.rs` (rewrite), `replay/tests/thread_isolation.rs` (adjust)

**Interfaces:**
- Consumes: `timeline::{load_user, UserSlice}`, `engine::{run_timeline, Baseline}`, `snapshot::SnapshotSource`.
- Produces: unchanged `reconcile::{ReconRow, reconcile_user}` — `reconcile_user(conn: &duckdb::Connection, backend_account_id: i64, products: &[Product], snapshot: &dyn SnapshotSource) -> Result<ReconRow>`. `ReconRow` field names/order unchanged (`account_id` now carries `backend_account_id`).

- [ ] **Step 1: `snapshot.rs`** — `DbSnapshotSource` currently holds a thread-local `rusqlite` read connection keyed by db path. Change the connection type to `duckdb::Connection` (open with `db::open_read`). The `SnapshotSource::raw_account` body: `SELECT state_json FROM snapshots WHERE backend_account_id = ?` → parse via the existing `account_state_json_to_raw`. `ArchivalRpcSnapshotSource` is unchanged. Keep the trait signature `fn raw_account(&self, account_id: i64, near_account_id: &str) -> Result<Option<Vec<u8>>>`.

- [ ] **Step 2: `reconcile.rs`** — in `reconcile_user`:
  - `load_user` now returns the richer `UserSlice`; use `slice.backend_account_id` where it used `slice.account_id`, `slice.existed_at_start` to decide the `no_baseline` semantics (an `existed_at_start` account with no snapshot row and no `--archival` → `no_baseline`; a `!existed_at_start` account legitimately starts empty → **not** `no_baseline`, expect `ok`).
  - Build `Baseline { account_id, raw_account, timezone_ms: slice.timezone_ms }`.
  - The status match: replace the old `no_baseline` gate. New logic:
    ```rust
    let no_baseline = slice.existed_at_start && raw_account.is_none();
    ```
    and keep the rest (`ReplayStatus::Error(msg) if no_baseline && msg.contains("is not found") => "no_baseline"`, etc.).
  - `ReconRow.account_id = slice.backend_account_id`.

- [ ] **Step 3: `run.rs`** — `build_worklist` query becomes
  `SELECT backend_account_id FROM accounts ORDER BY backend_account_id`; `--accounts`
  filter and `--shard` / `--sample` unchanged (operate on the returned ids).
  `db::open_read` now yields a `duckdb::Connection`; worker threads each call it.
  `RunOpts` unchanged. Keep worker thread names (`replay-worker-{k}`), the
  `install_quiet_panic_hook`, the writer thread.

- [ ] **Step 4: Rewrite `replay/tests/reconcile.rs`:**
  ```rust
  mod fixtures;
  use fixtures::write_fixture_dataset;
  use replay::db;
  use replay::db::ingest::{build_db, BuildOpts};
  use replay::products::load_products;
  use replay::reconcile::reconcile_user;
  use replay::snapshot::DbSnapshotSource;

  #[test]
  fn fresh_account_reconciles_without_baseline_flag() {
      let d = tempfile::tempdir().unwrap();
      let src = d.path().join("src");
      write_fixture_dataset(&src);
      let dbp = d.path().join("r.duckdb");
      let mut c = db::open_write(&dbp).unwrap();
      db::schema::init_schema(&c).unwrap();
      build_db(&mut c, &BuildOpts { source_dir: &src, accounts: None, sample: None }).unwrap();
      drop(c);

      let c = db::open_read(&dbp).unwrap();
      let products = load_products(std::path::Path::new("tests/fixtures/products.json")).unwrap();
      let snap = DbSnapshotSource::new(&dbp);
      let row = reconcile_user(&c, 300, &products, &snap).unwrap();
      // account 300: deposit then full withdraw_all -> some claimed interest, status ok
      assert_eq!(row.status, "ok");
  }
  ```
  (`tests/fixtures/products.json` must contain `365d_12apy` and
  `steps_365d_20000_10000_tiered_v1` — it already has the former from the old
  fixture; add the tiered product entry if missing, copying real terms from
  `test_data/products.json`.)

- [ ] **Step 5: `thread_isolation.rs`** — update its fixture construction to the parquet path; keep the assertion that N threads over the same DB produce the same rows as 1 thread.

- [ ] **Step 6: Run** `cargo test -p replay --profile release-replay --test reconcile --test thread_isolation` → PASS.

- [ ] **Step 7: Commit** — `git add replay/src/reconcile.rs replay/src/run.rs replay/src/snapshot.rs replay/tests/reconcile.rs replay/tests/thread_isolation.rs && git commit -m "feat(replay): reconcile/run/snapshot on duckdb + authoritative timezone"`

---

### Task 7: `explain.rs` — on-chain per-claim from the events table

**Files:**
- Modify: `replay/src/explain.rs`
- Modify: `replay/src/cli.rs` (`Explain.account` stays `i64`, doc tweak), `replay/src/main.rs` (unchanged arm)

**Interfaces:**
- Consumes: `timeline::load_user`, `payload::parse_event`, `engine::run_timeline`.

- [ ] **Step 1: Rewrite the on-chain map** — in `explain`, replace the `jar_events` query with:
  ```rust
  let mut stmt = conn.prepare(
      "SELECT ts_ms, payload FROM events WHERE backend_account_id = ?1 AND event = 'claim' ORDER BY ts_ms",
  )?;
  let rows = stmt.query_map(duckdb::params![opts.account], |r| {
      Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?))
  })?;
  let mut onchain: BTreeMap<u64, u128> = BTreeMap::new();
  for row in rows {
      let (ts, payload) = row?;
      if let Some(crate::payload::ParsedEvent::Claim { total }) =
          crate::payload::parse_event("claim", None, &payload)?
      {
          *onchain.entry(ts).or_default() += total;
      }
  }
  ```
- [ ] **Step 2: `Baseline`** — build with `timezone_ms: slice.timezone_ms`; use `slice.backend_account_id`.
- [ ] **Step 3: Run** `cargo build -p replay --profile release-replay` and a manual smoke against a real `--db` if available (not gated in CI).
- [ ] **Step 4: Commit** — `git add replay/src/explain.rs replay/src/cli.rs && git commit -m "feat(replay): explain reads on-chain claims from the events table"`

---

### Task 8: End-to-end test + old-fixture cleanup

**Files:**
- Rewrite: `replay/tests/run_e2e.rs`
- Delete: `replay/tests/fixtures/{jar_events.csv,step_packages.csv,boosted_step_packages.csv,max_subscriptions.csv,users.csv,snapshots.ndjson}`
- Modify: `replay/tests/build_db.rs` (already rewritten), remove any lingering `--only` references

**Interfaces:**
- Consumes: `run::{run, RunOpts, parse_shard}`, `fixtures::write_fixture_dataset`.

- [ ] **Step 1: Rewrite `run_e2e.rs`** to build the parquet fixture DB and run reconciliation with `threads = 2` and `threads = 1`, asserting:
  - `summary.processed == 3`
  - header row exact: `account_id,near_account_id,calculated_total_claim,actual_total_claim,delta,rel_delta,n_claims,status`
  - `threads = 1` is byte-deterministic across two runs
  - `--shard (0,2)` + `(1,2)` partition the 3 accounts with no overlap
  - `--sample 1` → `processed == 1`
  Model the assertions on the existing `run_e2e.rs` (keep its structure, swap the fixture).

- [ ] **Step 2: Delete** the stale CSV/ndjson fixtures listed above. Keep `replay/tests/fixtures/products.json`.

- [ ] **Step 3: `grep -rn "jar_events\|step_packages\|max_subscriptions\|--only\|rusqlite" replay/`** — every hit must be gone or intentional. Fix stragglers.

- [ ] **Step 4: Run the full replay suite** — `cargo test -p replay --profile release-replay` → all green.

- [ ] **Step 5: Commit** — `git add -A replay/tests && git commit -m "test(replay): end-to-end suite on the parquet fixture; drop CSV fixtures"`

---

### Task 9: `parse.rs` prune, README, machete/clippy

**Files:**
- Modify: `replay/src/parse.rs` (remove now-dead CSV timestamp parsers if unused)
- Modify: `replay/Cargo.toml` (drop `csv` if unused)
- Rewrite: `replay/README.md`
- Modify: `.gitignore` (already has `/test_data/interest_replay`), `replay/src/db/schema.rs` doc

**Interfaces:** none new.

- [ ] **Step 1: `grep -rn "csv::" replay/src` and `grep -rn "space_utc_to_epoch_ms\|iso8601_ms_to_epoch_ms\|timestamp_to_epoch_ms" replay/`** — if unused, delete those `parse.rs` functions and their tests, keep `H_MS`, `H_BLOCK`, `T_END_MS`, `yocto_str_to_u128`. Remove `csv` from `Cargo.toml` `[dependencies]` if no `csv::` remains.

- [ ] **Step 2: `cargo machete`** (from repo root) → must not report `replay`. `cargo clippy -p replay --profile release-replay -- -D warnings` → clean (fix warnings inline).

- [ ] **Step 3: Rewrite `replay/README.md`** — new input (`test_data/interest_replay/` parquet: `events/`, `accounts/`, `account_timezones/`), `build-db --source <dir>` → `.duckdb`, `run --db x.duckdb --out rec.csv --archival`, `explain --db x.duckdb --account N`. Document: DuckDB requirement is vendored (`duckdb` crate `bundled`, no external binary); the `set_timezone`-upfront behaviour; known divergences (contract-version drift across 4.1.0–4.2.2, `restake_all` sweeping non-named matured jars, `apply_booster` rejected rows ignored). Drop all step-package / subscription / `--only` docs.

- [ ] **Step 4: Run** `cargo test -p replay --profile release-replay` + `cargo test -p sweat_jar --features replay-engine replay_account_history -- --nocapture | grep "TOTAL CLAIMED"` (still `430841686064034204316387`).

- [ ] **Step 5: Commit** — `git add replay/src/parse.rs replay/Cargo.toml replay/README.md replay/src/db/schema.rs && git commit -m "chore(replay): prune dead CSV parsing; refresh README"`

---

### Task 10: Real-data smoke (manual, no CI)

**Files:** none (produces a scratch report).

- [ ] **Step 1: Build** `cargo build -p replay --profile release-replay`.
- [ ] **Step 2:** `./target/release-replay/replay build-db --db /tmp/ir.duckdb --source test_data/interest_replay --sample 3000` — expect `accounts: 3000`, `events: <N>` printed, no error.
- [ ] **Step 3:** `./target/release-replay/replay run --db /tmp/ir.duckdb --out /tmp/ir.csv --threads 10 --archival` — expect no `fatal runtime error`, all rows written.
- [ ] **Step 4:** Summarize in the ledger: status distribution, `sum_calculated/sum_actual`, count of negative deltas and worst `rel_delta`, compared against the pre-pivot baseline (300-account archival sample: 84 exact / 197 positive / 10 negative, worst −0.34%). Spot-check 2–3 previously-negative accounts (`989`, `937`) with `explain`.
- [ ] **Step 5:** No commit (scratch artifacts only). Report findings to the user.

---

## Self-review notes

- **Spec coverage:** DB swap (T1), payload parsing (T2), build-db (T3), engine actions + timezone (T4), timeline (T5), reconcile/run/snapshot (T6), explain (T7), e2e + cleanup (T8/T9), real smoke (T10). All spec sections mapped.
- **Golden safety:** engine changes are additive; T4 Step 8 and T9 Step 4 re-assert `430841686064034204316387`.
- **Type consistency:** `backend_account_id: i64` throughout; `ReconRow.account_id` carries it; `Baseline.timezone_ms: Option<i64>`; `ParsedEvent` variants match the `Action` mapping in T5.
- **Risk — `duckdb` bundled build time:** first compile adds ~1–2 min. If the crate fails to build on this toolchain, fall back to invoking the `duckdb` CLI (brew, pinned) from `build-db` to emit a SQLite file and keep `rusqlite` for reads — this is a T1/T3 pivot only, the rest of the plan is unchanged.
- **Risk — `restake_all` semantics:** documented divergence (T9 Step 3); acceptable per spec "Out of scope".
