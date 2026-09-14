# Multi-user replay reconciliation

Status: design approved 2026-09-08

## Goal

For every Sweat user, re-simulate their jar activity over a fixed window against
the local contract and compare the **replayed total claimed** against the
**on-chain total claimed** recorded in the indexer. Emit a per-user report of
deltas so we can find accounts whose interest math diverges.

This scales the existing single-account `contract/src/replay.rs` test
(`replay_account_history`) to ~1.2M users and the full event history, backed by a
SQLite database instead of in-memory CSV parsing.

## Window

Same fixed bounds as `REPLAY_DATA_SPEC.md` / the current replay test:

| bound | value |
|-------|-------|
| start `H` | block `190375496` = `2026-03-20T14:41:50.156Z` = `1774017710156` ms |
| end `T_end` | `2026-08-31T11:10:57.961Z` = `1788174657961` ms |

- Baseline account state = state at `H`.
- All events filtered to `(H, T_end]` (strictly after `H`, up to and including `T_end`).

## Inputs

### CSV source files (`test_data/`)

| file | rows (approx) | key columns |
|------|---------------|-------------|
| `users.csv` | 1.2M | `account_id`, `near_account_id`, `sweatcoin_user_id` |
| `jar_events.csv` | 8.0M | `account_id`, `jar_id`, `product_id`, `product_name`, `near_block_timestamp`, `event_type`, `amount`, `fee_amount`, `deposit_ids` |
| `step_packages.csv` | 285M | `account_id`, `created_at`, `steps` |
| `max_subscriptions.csv` | 47K | `user_id`, `datetime`, `action_type` |

**Join key**: `account_id` is the same integer across all four files
(`users.account_id == jar_events.account_id == step_packages.account_id ==
max_subscriptions.user_id`). `near_account_id` (64-hex) is the on-chain
`AccountId`. `sweatcoin_user_id` is reference-only and is **not** ingested.

`event_type` values in `jar_events.csv`: `deposit`, `claim`, `withdraw`,
`restake`, `merge`. `amount` is in yocto-with-18-decimals (the SWEAT token base
unit used throughout the contract). `merge` rows carry `amount: 0`.

`action_type` values in `max_subscriptions.csv`: `subscribed`, `expired`.

### Products

`replay fetch-products` calls `get_products()` on `v2.jars.sweat` via plain JSON
RPC (`https://rpc.mainnet.near.org`, `call_function`, `args_base64: e30=`) and
writes the result to `test_data/products.json` (committed to the repo). The
engine loads all products from that file.

**Known limitation**: this is the *current* product configuration, not the
configuration as it stood during the window. APY changes, cap changes, and
enable/disable toggles that happened inside `(H, T_end]` are not modeled. If this
proves material, a later iteration can snapshot product config per-block via
archival RPC.

Restaking permission is not present in the `get_products()` payload; in the
current contract model it is implied by the terms type (fixed-term products allow
restaking). The engine derives it the same way the contract does.

### Baseline snapshot (placeholder in this iteration)

Each user's account state at block `H` (jars, deposits, principal, score, cache,
`increased_apy` / `increased_score_cap` flags, timezone). We do **not** have this
data yet; it will come from an archival-RPC extraction at block `H`, added later.

This iteration ships the seam, not the extractor:

```rust
// replay/src/snapshot.rs
trait SnapshotSource {
    fn account_state(&self, near_id: &str) -> Option<AccountState>;
}

struct FileSnapshotSource;      // reads test_data/snapshots.ndjson
struct ArchivalRpcSnapshotSource; // todo!() — call sketched, unimplemented
```

`AccountState` mirrors the `account_state` + `products_referenced` shape of the
existing `test_data/account_full_state_190375496.json`.
`test_data/snapshots.ndjson` is one JSON object per line, keyed by
`near_account_id`. `build-db` ingests it into the `snapshots` table when the file
is present.

**Accounts with no snapshot** start from a fresh empty account (no jars). The
`AccountScore::default()` timestamp hazard applies — the engine sets the score
`updated_at` explicitly to `H` rather than relying on `default()`, which stamps
current block time.

## Architecture

### Crate layout

New workspace member **`replay/`** — a binary crate. Added to `members` in the
root `Cargo.toml` (not `default-members`, so `cargo build`/`cargo test` in CI are
unaffected).

Dependencies: `rusqlite` (with the `bundled` feature), `csv`, `clap` (derive),
`serde` / `serde_json`, `anyhow`, a thread pool (`rayon` or `std::thread`
scoped), and `sweat-jar` (the `contract` crate) with `features = ["replay-engine"]`.

### Engine — lives in `contract`, behind a feature

The simulation core lives in the `contract` crate so both the existing
single-account test and the new binary use one implementation, with **no
dependency cycle** (`replay/` depends on `contract`, never the reverse).

New feature `replay-engine` in `contract/Cargo.toml`. Under
`#[cfg(any(test, feature = "replay-engine"))]`:

- `contract/src/replay/engine.rs`:
  - `struct Timeline { events: Vec<Event> }`
  - `struct Event { ts_ms: u64, seq: u64, action: Action }`
  - `enum Action { RecordScore(Score), Deposit { product_id, amount }, Withdraw { product_id, amount }, Restake { product_id }, SetIncreasedScoreCap(bool), Claim }`
  - `fn run_timeline(baseline: AccountState, products: &[Product], timeline: Timeline) -> ReplayOutcome`
  - `struct ReplayOutcome { total_claimed: u128, per_claim: Vec<(u64, u128)>, status: ReplayStatus }`
  - `enum ReplayStatus { Ok, Error(String) }`
- The feature re-exports the test helpers the engine needs as `pub`:
  `common::testing::Context`, `common::env::test_env_ext`,
  `migration::api::store_account_raw`. (Audit exactly which symbols during
  implementation; keep the surface minimal.)

`contract/src/replay.rs` (the existing test) keeps its single-account file
parsers but delegates the simulation loop to `engine::run_timeline`. It stays a
fast smoke test and pins the known total for account `3d0708…` as a regression
guard.

### `run_timeline` behavior

1. Reset the thread-local near-sdk mock env and storage.
2. Build a `Context` with the product catalogue. Public-key checks disabled —
   deposits are submitted unsigned (we exercise interest math, not signatures).
3. Store the baseline account via `store_account_raw` (or start empty).
4. Set block time to `H`.
5. For each event in order: set block time to `event.ts_ms`, then dispatch:
   - `RecordScore` — operator calls `record_score([(near_id, [(score, UTC(ts))])])`
   - `Deposit` — `deposit(near_id, DepositTicket { product_id, valid_until: 0, timezone: None }, amount, None)`
   - `Withdraw` — `withdraw` for that account / product
   - `Restake` — `restake_all` for that account (matured principal → fresh deposit at `ts`)
   - `SetIncreasedScoreCap` — operator calls `set_feature_enabled(near_id, IncreasedScoreCap, enabled)`
   - `Claim` — user calls `claim_total(None)`; record `(ts, claimed_total)`
6. Return `ReplayOutcome`.

The whole per-user call is wrapped in `std::panic::catch_unwind` by the driver
(see below), so a contract `panic!` on an unexpected sequence (e.g. withdraw on a
locked jar) marks that one user `status = Error` instead of aborting the run.

## SQLite database

One file, built once by `build-db`, read concurrently by `run`.

### Schema

```sql
CREATE TABLE users (
    account_id       INTEGER PRIMARY KEY,
    near_account_id  TEXT NOT NULL
);

CREATE TABLE jar_events (
    account_id  INTEGER NOT NULL,
    ts_ms       INTEGER NOT NULL,
    seq         INTEGER NOT NULL,     -- source file line order, tie-breaker
    event_type  TEXT NOT NULL,        -- deposit | claim | withdraw | restake
    product_id  TEXT NOT NULL,
    amount      TEXT NOT NULL         -- u128 as decimal string
);

CREATE TABLE step_packages (
    account_id  INTEGER NOT NULL,
    ts_ms       INTEGER NOT NULL,
    steps       INTEGER NOT NULL      -- clamped to u16 range at ingest
);

CREATE TABLE subscriptions (
    account_id  INTEGER NOT NULL,
    ts_ms       INTEGER NOT NULL,
    active      INTEGER NOT NULL      -- subscribed = 1, expired = 0
);

CREATE TABLE snapshots (
    account_id  INTEGER PRIMARY KEY,
    state_json  TEXT NOT NULL
);

CREATE TABLE meta (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL              -- window bounds, build timestamp, source file hashes
);
```

Indexes, created **after** bulk load:

```sql
CREATE INDEX ix_jar_events_acct ON jar_events (account_id, ts_ms, seq);
CREATE INDEX ix_step_packages_acct ON step_packages (account_id, ts_ms);
CREATE INDEX ix_subscriptions_acct ON subscriptions (account_id, ts_ms);
```

### Ingest rules

- `merge` rows dropped. `deposit_ids`, `fee_amount`, `product_name` dropped.
- All event rows filtered to `(H, T_end]` at ingest.
- `jar_events.near_block_timestamp` (ISO-8601 with ms, `Z`) → epoch ms.
- `step_packages.created_at` (`YYYY-MM-DD HH:MM:SS UTC`) → epoch ms.
  `steps` clamped to `65535`.
- `max_subscriptions.datetime` (ISO-8601) → epoch ms; `subscribed`→1, `expired`→0.
- Build pragmas: `PRAGMA synchronous = OFF; PRAGMA journal_mode = MEMORY;`
  one transaction per table.

### Size / cost

`step_packages` dominates: ~285M rows of `(int, int, int)`, on the order of
10–20 GB in SQLite with its index. One-time build cost is tens of minutes. Dev
iteration avoids this via `--only` and `--sample` / `--accounts` (below).

## CLI (`replay` binary)

```
replay fetch-products --out test_data/products.json

replay build-db --db replay.db
    [--test-data-dir test_data]
    [--only users,jar_events,step_packages,subscriptions,snapshots]
    [--accounts FILE]     # newline-separated account_ids; ingest only these
    [--sample N]          # ingest only the first N users (+ their events)

replay run --db replay.db --out reconciliation.csv
    [--products test_data/products.json]
    [--threads N]         # default: CPU count
    [--shard I/N]         # process only users where account_id % N == I
    [--accounts FILE]
    [--sample N]
    [--tolerance 1e-6]
```

## Event synthesis (`run`, per user)

Pull all rows for one `account_id` from `jar_events`, `step_packages`,
`subscriptions`. Merge into one `Vec<Event>` sorted by `(ts_ms, rank, seq)`:

| source row | action |
|---|---|
| `jar_events` `deposit` | `Deposit { product_id, amount }` |
| `jar_events` `withdraw` | `Withdraw { product_id, amount }` |
| `jar_events` `restake` | `Restake { product_id }` |
| `jar_events` `claim` | `Claim` — **one per distinct `ts_ms`**, not one per jar |
| `step_packages` row | `RecordScore(steps)` |
| `subscriptions` row | `SetIncreasedScoreCap(active)` |

`rank` for same-millisecond tie-break:

| rank | actions |
|---|---|
| 0 | `RecordScore` |
| 1 | `Deposit`, `Withdraw`, `Restake`, `SetIncreasedScoreCap` |
| 2 | `Claim` (so a claim sees fully up-to-date state) |

`seq` (jar_events source line order) breaks remaining ties.

`increased_apy` is taken from the baseline snapshot and never changes during
replay — subscriptions only toggle `increased_score_cap`.

## Reconciliation & report

Per user, the driver:

1. Loads baseline via `SnapshotSource` (empty account if absent).
2. Builds the timeline.
3. `catch_unwind(|| run_timeline(...))`.
4. Computes:
   - `calculated_total_claim` = Σ of the amounts returned by each replayed
     `claim_total(None)` call.
   - `actual_total_claim` = Σ `jar_events.amount` where `event_type = 'claim'`
     for this account in the window.
   - `delta = calculated_total_claim − actual_total_claim` (signed, i128).
   - `rel_delta = delta / actual_total_claim` (0 when actual is 0).
   - `n_claims` = number of distinct claim timestamps replayed.
   - `status` = `ok` | `error` (panic caught) | `no_baseline` (informational).

### `reconciliation.csv`

One row per user:

```
account_id,near_account_id,calculated_total_claim,actual_total_claim,delta,rel_delta,n_claims,status
```

Threads append rows through a channel to a single CSV writer; `--shard` runs
write disjoint files that concatenate.

### Summary (stdout)

- Total users processed, counts by `status`.
- Σ `calculated_total_claim`, Σ `actual_total_claim`, aggregate delta.
- Count of users with `|rel_delta| > tolerance`.
- The 20 largest `|delta|` offenders.

## Threading

`run` uses a thread pool sized by `--threads` (default = CPU count). near-sdk's
mock blockchain env is **thread-local**, so worker threads are naturally
isolated: each user is an independent job, and a worker resets its thread-local
env + storage at the start of each job. Each worker opens its own read-only
SQLite connection (`OpenFlags::SQLITE_OPEN_READ_ONLY`).

`--shard I/N` sits on top for multi-process / multi-machine fan-out over one
read-only DB file.

## Testing

- **Unit**: timestamp/amount/datetime parsers (moved out of `replay.rs`), event
  merge + ordering (`rank`/`seq` tie-breaks), reconciliation aggregation
  (`delta`, `rel_delta`, `status`).
- **Golden**: `contract/src/replay.rs` keeps the pinned total for account
  `3d0708…` as a regression guard against engine changes.
- **Integration**: `build-db --sample 50` then `run --sample 50`; assert the DB
  has the expected tables, the report has 50 rows with the exact header, and the
  summary totals are internally consistent.
- **Thread-isolation test**: run two users on one worker thread back-to-back
  where user A creates jars and user B has none; assert B's outcome is unaffected
  by A (guards the storage-reset requirement).

## Risks

1. **Mock storage carryover between users.** near-sdk `testing_env!` persists
   thread-local storage across calls on one thread. Mitigation: explicit env +
   storage reset at the start of every per-user job; thread-isolation test
   above. This is the top implementation risk.
2. **`step_packages` ingest cost / DB size** (~285M rows). Mitigation: build
   pragmas, post-load indexing, `--only` / `--sample` / `--accounts` for dev.
3. **Contract panics on unexpected sequences** (withdraw on locked jar, claim
   with nothing, restake of a non-restakable product). Mitigation: per-user
   `catch_unwind`; `status = error` row instead of aborting the run.
4. **Historical product/APY drift not modeled** — `get_products()` is current
   state only. Documented limitation; revisit with per-block product snapshots
   if deltas point at it.
5. **No baseline snapshot yet.** Until the archival-RPC extractor lands, every
   pre-`H` jar holder replays from empty and will show large negative deltas.
   `FileSnapshotSource` + `snapshots.ndjson` is the drop-in path;
   `status = no_baseline` flags these rows.

## Out of scope

- The archival-RPC baseline extractor (seam only this iteration).
- Per-block historical product configuration.
- Modeling `merge` events (no-op in the merged-jars model).
- Signature / public-key verification on deposits.
