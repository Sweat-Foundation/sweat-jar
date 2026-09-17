# Event-Sourced Replay — Design

**Status:** approved (verbal, 2026-09-10)

## Problem

The `replay/` reconciliation tool synthesizes the contract event stream from
partial extracts (`jar_events.csv` + `step_packages.csv` +
`boosted_step_packages.csv` + `max_subscriptions.csv`). The synthesis has
irreducible error: it guesses `record_score` cadence, cannot see oracle tx
execution delay, never modelled `apply_booster`, and forces `Timezone(0)` on
score-jar deposits when the baseline timezone is unset.

A new export, `test_data/interest_replay/`, contains the **exhaustive** on-chain
event stream for every affected account, plus resolved per-account timezones.
Pivot the tool to replay these events directly.

## Input data (`test_data/interest_replay/`, gitignored, ~25 GB)

DuckDB reads the parquet natively.

### `accounts/*.parquet` — 1,308,003 rows
Relevant columns: `backend_account_id` (bigint, PK), `near_account_id` (varchar),
`existed_at_start` (bool — needs an archival baseline at block H),
`created_in_window` (bool).

### `account_timezones/*.parquet` — 1,308,003 rows (1:1 with accounts)
`backend_account_id`, `near_account_id`, `timezone_ms` (bigint — ms offset from
UTC; NULL for 2,936 accounts that genuinely never set one), `tz_source`.

### `events/*.parquet` — ~259 M rows, 2026-03-20 14:41:56 → 2026-08-31
Columns used: `backend_account_id`, `block_timestamp_utc` (timestamp, µs —
**actual execution time**), `log_index` (bigint — intra-tx order), `receipt_status`
(keep only `SUCCESS_VALUE`; 16,519 `FAILURE` rows dropped), `event` (varchar),
`payload` (varchar JSON). `contract_version` is retained for reference only — the
engine runs current contract code against all five historical versions and the
resulting drift is accepted.

Event types and payload shapes:

| `event` | rows | `payload` JSON |
|---|---|---|
| `record_score` | 251 M | `[[score:int, ts_ms:int], …]` (0–160 pairs; may be `[]`) |
| `claim` | 3.68 M | `["<hash>", {"items": [["<product>", "<yocto>"], …], "timestamp": int}]` |
| `apply_booster` | 2.9 M | `{"timestamp": "<ms>", "score": "<int>"}` — **`role` column** is `applied` or `rejected`; replay only `applied` |
| `deposit` | 1.26 M | `["<hash>", ["<product>", "<yocto>"]]` |
| `withdraw_all` | 78 k | `["<hash>", [["<product>", "<fee>", "<yocto>"], …]]` |
| `restake` | 60 k | `["<hash>", {"from": ["<product>", …], "into": "<product>", "is_success": bool, "restaked": "<yocto>", "withdrawn": "<yocto>", "timestamp": int}]` |
| `set_feature_enabled` | 20 k | `["<hash>", "increased_score_cap", bool]` (only ever this feature) |

## Architecture

Keep: the `sweat_jar` `replay-engine` feature, `engine::run_timeline`, the
archival baseline (`ArchivalRpcSnapshotSource`), the threaded driver in `run.rs`,
the `reconciliation.csv` output shape, `explain`.

Replace: the entire input layer — DB backend, `build-db`, `timeline.rs`, and the
`parse.rs` CSV helpers.

### Database — DuckDB

Swap `rusqlite` → the `duckdb` crate (`bundled`). `build-db` reads the parquet
dirs and writes a single `.duckdb` file:

- **`events`**: `backend_account_id BIGINT`, `ts_ms BIGINT` (`epoch_ms(block_timestamp_utc)`),
  `log_index BIGINT`, `event VARCHAR`, `payload VARCHAR`. Built
  `WHERE receipt_status = 'SUCCESS_VALUE'`, `ORDER BY backend_account_id, ts_ms, log_index`
  so per-account range scans hit the row-group zonemap.
- **`accounts`**: `backend_account_id BIGINT PRIMARY KEY`, `near_account_id VARCHAR`,
  `existed_at_start BOOLEAN`, `timezone_ms BIGINT` (NULL preserved). Join of
  `accounts/` and `account_timezones/`.
- **`snapshots`**: `backend_account_id BIGINT PRIMARY KEY`, `state_json VARCHAR`.
  Optional, unpopulated by `build-db` (archival is the baseline path).
- **`meta`**: `key VARCHAR PRIMARY KEY`, `value VARCHAR` — `source_dir`, `built_at`,
  `h_ms`, `t_end_ms`, `events_rows`, `accounts_rows`.

`--accounts <file>` and `--sample N` filter which accounts' events are copied.

### Payload parsing — `replay/src/payload.rs` (new)

`serde_json` structs + one `parse_event(event: &str, payload: &str) -> Result<Option<ParsedEvent>>`
returning `None` for events that map to no action (`record_score` with `[]`,
`apply_booster` when caller passes a `rejected` row, `restake` with
`is_success = false`). Yocto strings → `u128` via the existing
`parse::yocto_str_to_u128`.

### Timeline synthesis — `timeline.rs` rewrite

`load_user(conn, backend_account_id) -> (UserSlice, Timeline)`:
`SELECT ts_ms, log_index, event, payload FROM events WHERE backend_account_id = ?1 ORDER BY ts_ms, log_index`,
map each row via `payload.rs`, `seq = log_index`. Event → `Action`:

| event | `Action` |
|---|---|
| `record_score` | `RecordScore(pairs)` |
| `apply_booster` (`role = applied`) | `ApplyBooster { score, timestamp_ms }` |
| `deposit` | `Deposit { product_id, amount }` |
| `withdraw_all` | `WithdrawAll { product_ids }` (from payload list) |
| `restake` (`is_success`) | `RestakeAll { into, amount: restaked }`; if `from == [into]` → `Restake { product_id: into, amount: restaked }` |
| `set_feature_enabled` | `SetIncreasedScoreCap(value)` |
| `claim` | `Claim`; add `items` sum to `UserSlice.onchain_claimed` |

`role` is not a column in the `events` table as specified above — add
`role VARCHAR` to the `events` table so `apply_booster` rows can be filtered.

### Engine — additive changes to `contract/src/replay/engine.rs`

New `Action` variants (existing ones stay, so the golden
`replay_account_history` test — `TOTAL CLAIMED: 430841686064034204316387` — is
untouched):

- `ApplyBooster { score: Score, timestamp_ms: u64 }` →
  `context.switch_account_to_operator(); context.contract().apply_booster(vec![account_id.clone()], score, UTC(timestamp_ms))`
- `WithdrawAll { product_ids: Vec<String> }` →
  `context.switch_account(&account_id); let _ = context.contract().withdraw_all(Some(product_ids.into_iter().collect()))`
- `RestakeAll { product_id: String, amount: u128 }` →
  `context.switch_account(&account_id); let _ = context.contract().restake_all(ticket_for(&product_id), None, Some(amount.into()))`

`Baseline` gains `timezone_ms: Option<i64>`. `run_timeline`, after installing the
baseline and before the event loop: if `Some(tz)` and `tz != i64::MIN`,
`context.switch_account_to_operator(); context.contract().set_timezone(account_id.clone(), tz.into())`.

### Reconcile / run / explain

`reconcile_user` passes `slice`-derived `timezone_ms` into `Baseline`. `ReconRow`
shape unchanged. `explain` reads on-chain per-claim totals from the `events`
table (claim payload `items`), not the old `jar_events` table. `snapshot.rs`
`DbSnapshotSource` switches to the `duckdb` connection.

### CLI

`build-db`: `--source <dir>` (default `test_data/interest_replay`) replaces
`--test-data-dir`; `--only` is removed. `run` and `explain` unchanged except
`--db` now names a `.duckdb`.

## Testing

Fixtures generated at test time (no parquet blobs in git): a
`replay/tests/fixtures.rs` helper writes tiny `events/`, `accounts/`,
`account_timezones/` parquet from SQL literals via a `duckdb` in-memory
connection `COPY … TO … (FORMAT parquet)`. Covers: one `existed_at_start`
account with a score jar + booster + claim, one fresh account, one account with
`withdraw_all`, one with `restake`. Golden contract test must stay at
`430841686064034204316387`. `cargo machete` / `clippy` clean.

## Out of scope

Per-version contract behaviour; `apply_booster` `rejected` modelling; partial
`restake` where `from` spans multiple jars but not all matured jars (uses
`restake_all` with the exact `restaked` amount — the rest of matured principal is
withdrawn, a documented divergence).
