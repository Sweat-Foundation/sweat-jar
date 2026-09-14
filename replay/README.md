# `replay` — per-account yield reconciliation

## Purpose

`replay` replays each affected account's **exhaustive on-chain event stream**
against the local contract engine and compares the **replayed total claim**
against the **on-chain total claim**. It emits one CSV row per account with the
signed delta so accounts whose interest math diverges can be found.

This is the population-scale counterpart of the single-account
`replay_account_history` golden test in `contract/src/replay.rs`.

## Window

| bound | value | meaning |
|-------|-------|---------|
| `H` (start) | `1774017710156` ms | block `190375496` — baseline account state is "as of `H`" |
| `T_end` (end) | `1788174657961` ms | end of the replay window |

The bounds live in `replay/src/parse.rs` (`H_MS`, `H_BLOCK`, `T_END_MS`) and are
written into the `meta` table at build time.

## Input

`test_data/interest_replay/` — gitignored, ~25 GB, three parquet directories:

| dir | columns | notes |
|-----|---------|-------|
| `events/` | `backend_account_id, block_timestamp_utc, log_index, receipt_status, event, role, payload` | `payload` is a JSON string. Only `receipt_status = 'SUCCESS_VALUE'` rows are ingested. Event types: `record_score`, `claim`, `apply_booster` (role `applied`/`rejected`), `deposit`, `withdraw_all`, `restake`, `set_feature_enabled`. |
| `accounts/` | `backend_account_id, near_account_id, existed_at_start, created_in_window` | `existed_at_start` accounts need an archival baseline at block `H`. |
| `account_timezones/` | `backend_account_id, timezone_ms` | authoritative per-account timezone; `NULL` for a handful that never set one. |

## Commands

Build with the **`release-replay`** profile (see [Build/run](#buildrun)):

```sh
cargo build -p replay --profile release-replay
BIN=./target/release-replay/replay
```

### `build-db`

```
replay build-db --db x.duckdb --source test_data/interest_replay [--accounts file] [--sample N]
```

Builds a sorted `.duckdb` with the `events`, `accounts`, `snapshots`, and `meta`
tables. `--accounts <file>` keeps only the listed `backend_account_id`s (one
integer per line, blank lines and `#` comments ignored); `--sample N` keeps only
the first `N` accounts. `--accounts` beats `--sample`.

### `run`

```
replay run --db x.duckdb [--out reconciliation.csv]
  [--threads N] [--archival] [--archival-rpc-url URL]
  [--shard i/n] [--accounts file] [--sample N] [--tolerance f] [--force]
```

Threaded per-account reconciliation. Workers are named `replay-worker-*`, the DB
writer `replay-writer`. `--shard i/n` processes only accounts where
`backend_account_id.rem_euclid(n) == i` (multi-process / multi-machine fan-out
over one DB). `--tolerance` (default `1e-6`): an `ok` row with `|rel_delta|`
above it is counted in `over_tolerance`.

`--archival` fetches **every** account's block-`H` state live from a NEAR
archival node (`get_account` on `v2.jars.sweat` at block `H_BLOCK`) —
`existed_at_start` is the export's own classification, not trusted ground
truth, so it no longer gates the fetch. A `null` response is a complete,
authoritative answer ("no state at H"), not a gap: the account's first
`Deposit` in its timeline creates it, exactly like the real contract's
`get_or_create_account_mut`. `--archival-rpc-url` overrides the endpoint.
Without `--archival` (the local `snapshots` table, normally unpopulated),
a missing row for an `existed_at_start` account IS a gap and is marked
`no_baseline` — that table isn't authoritative the way a live lookup is.

**`FASTNEAR_API_KEY`** — set it and every archival request goes out with
`Authorization: Bearer <key>`; unset (or blank) falls back to unauthenticated.
Worth having: the free tier throttles hard above ~4 concurrent requests, which
is the main thing capping `--threads` on a full-population run. The key is
read from the environment only — never a flag, never logged, and never
included in error messages (those carry the URL only), so it can't leak into
`results`/CSV output or a shared terminal transcript.

```sh
export FASTNEAR_API_KEY=...   # keep it out of shell history / committed files
replay run --db x.duckdb --archival --threads 16
```

**`REPLAY_TIMING=1`** makes `reconcile_user` print one `TIMING account=<id>
status=<s> load_us=<..> fetch_us=<..> replay_us=<..> total_us=<..>` line per
account to stderr — a load breakdown (DB load / snapshot fetch / engine
replay) for sizing a run before committing to it. `replay_us` scales
linearly with the account's event count (~9ms/event on this crate's
`release-replay` build — the near-sdk mock's per-call overhead dominates, not
the interest math); `fetch_us` is the archival RPC round-trip and is roughly
flat per account (~0.4s on FastNEAR). For a lot of accounts, replay CPU time
rivals or exceeds the archival fetch — worth measuring on a sample before
assuming the RPC is the only bottleneck.

**Results live in the database, not just the CSV.** Each `ReconRow` is upserted
into the `results` table (keyed by `backend_account_id`) as soon as it's
computed — a crash or a killed process loses at most the row currently in
flight, never the ones already done. **`run` is resumable by default**: the
worklist is `accounts` minus whatever's already in `results`, so re-running the
exact same command after an interruption (or just periodically, e.g. against a
`--shard`ed worklist run over several sessions) only computes what's still
missing. Pass `--force` to recompute everyone regardless. `--out`, if given, is
still written at the end — but it's a full export of `results` (in
`backend_account_id` order), not just what this invocation computed; a resumed
run with nothing left to do still (re-)writes a complete, up-to-date CSV. Omit
`--out` to update only the database and export later with `export-csv`.

### `export-csv`

```
replay export-csv --db x.duckdb --out reconciliation.csv
```

Writes the current `results` table to CSV without recomputing anything —
re-export after the fact, or after interrupting a `run`. DuckDB locks the file
for exclusive access while `run` holds it open, so a concurrent `export-csv`
from another process fails with a lock error; run it after `run` exits (Ctrl-C
included — the writer thread only holds one row's upsert at a time, so what's
already committed to `results` is safe to export).

### `explain`

```
replay explain --db x.duckdb --account <backend_account_id> [--archival]
```

Per-claim breakdown (calculated vs on-chain) for one account, to trace a non-zero
`delta`.

### `fetch-products`

```
replay fetch-products
```

Refreshes `test_data/products.json` from mainnet.

## Build/run

```sh
cargo build -p replay --profile release-replay
# -> target/release-replay/replay
```

Why the custom profile: `.cargo/config.toml` sets `panic = "abort"` on the plain
`release` profile (for the contract wasm), which makes `catch_unwind` a no-op —
the first contract panic would kill the whole run. `[profile.release-replay]` (in
the root `Cargo.toml`) is `release` + `panic = "unwind"` + `debug-assertions =
true`. The last is needed so `require!` expands to `assert!` (a plain unwinding
panic) rather than a nounwind abort.

DuckDB is vendored via the `duckdb` crate's `bundled` feature — no external
binary is needed. (The `duckdb` CLI is only handy for ad-hoc parquet
exploration.)

Add `--features corrected-score-window` to reconcile against "what should have
been paid" (current, fixed score-window logic) instead of "what was actually
paid" (the default — reproduces the pre-v4.2.3 bug, see Known divergences
below). It rebuilds `target/release-replay/replay` in place — the two modes
aren't both available at once from one build; run one, save its output, then
switch and rerun if you need both.

`replay` depends on `sweat_jar` with the `replay-engine` feature, which pulls
`near-sdk/unit-testing` — a **host-only** build. `replay` is not in the workspace
`default-members`, so plain `cargo build` / `cargo test` and CI are unaffected;
build it explicitly with `-p replay`.

## `reconciliation.csv`

```
account_id,near_account_id,calculated_total_claim,actual_total_claim,delta,rel_delta,n_claims,status
```

| column | meaning |
|--------|---------|
| `account_id` | = `backend_account_id` |
| `near_account_id` | on-chain `AccountId` |
| `calculated_total_claim` | Σ of the amounts returned by each replayed `claim` |
| `actual_total_claim` | Σ of the `claim` payload `items` recorded on-chain |
| `delta` | `calculated_total_claim − actual_total_claim` (signed) |
| `rel_delta` | `delta / actual_total_claim` (`0.0` when actual is `0`) |
| `n_claims` | number of claims replayed |
| `status` | `ok` \| `no_baseline` \| `error:<msg>` |

`status`: `ok` — replay succeeded from a real baseline; `no_baseline` — replay
succeeded but the account held jars before `H` and no archival baseline was
fetched (replayed from empty state; informational); `error:<msg>` — the engine or
snapshot parse failed for this account (`calculated` is `0`, `delta` is
`-actual`).

`run` prints a summary to stdout:

```
processed <n> | ok <n> | error <n> | no_baseline <n> | over_tolerance <n>
sum_calculated <n> | sum_actual <n>
```

## How the replay works

For each account:

1. Load the baseline — with `--archival`, `get_account` at block `H` for every
   account (`null` = confirmed empty, not a gap); without it, the local
   `snapshots` table.
2. Set the authoritative `timezone_ms` from `account_timezones/` (Oracle
   `set_timezone`) immediately before the account's first score-based jar is
   created — a deposit or restake into a `ScoreBased`/`TieredScoreBased`
   product — not upfront; a no-op for accounts with no score jar, and for a
   baseline that already carries a valid on-chain timezone.
3. Replay every event in `(block_timestamp_utc, log_index)` order, mapped to an
   engine `Action`:

   | event | `Action` |
   |-------|----------|
   | `record_score` | `RecordScore` |
   | `apply_booster` (role `applied`) | `ApplyBooster` |
   | `deposit` | `Deposit` |
   | `withdraw_all` | `WithdrawAll` |
   | `restake` | `Restake` / `RestakeAll` |
   | `set_feature_enabled` | `SetIncreasedScoreCap` |
   | `claim` | `Claim` |

4. Sum the `claim` payload `items` for the on-chain total.

## `db` schema

`replay/src/db/schema.rs`. Tables: `events` (`backend_account_id, ts_ms,
log_index, event, role, payload`), `accounts` (`backend_account_id,
near_account_id, existed_at_start, timezone_ms`), `snapshots`
(`backend_account_id, state_json`), `meta` (`key, value`), `results` (`run`'s
output — `backend_account_id` PK, the `reconciliation.csv` columns, plus
`computed_at`; see [`run`](#run)).

## Known divergences / limitations

- The engine runs **current** contract code; the window spans contract versions
  4.1.0–4.2.2, so version-specific historical behavior is not reproduced —
  **except** the one instance that turned out to dominate reconciliation error:
  `AccountScore::shift()`/`wipe()` are `replay-engine`-conditional (see
  `model/src/data/score/mod.rs`) to intentionally revert the v4.2.3 fix
  (`stamp score.updated_at when settle_interest rolls the window`), because
  the entire replay window predates that fix and every historical claim that
  raced the oracle's daily `record_score` lost a day of score accrual
  on-chain. The golden regression (`contract/src/replay/mod.rs`) pins a
  separate total for each build config accordingly.
- `restake` with a multi-jar `from` set is replayed as `restake_all` with the
  exact `restaked` amount — the rest of the account's matured principal is
  withdrawn, which can differ from a genuine single-jar restake.
- `apply_booster` rows with `role = rejected` are ignored (they had no on-chain
  effect).
- `reconcile_user` wraps `run_timeline` in its own `catch_unwind`: the near-sdk
  unit-test mock can let a second panic on one worker thread escape the
  engine's internal guard; the wrapper turns that account into an `error:` row
  rather than killing the worker.

## Testing

```sh
cargo test -p replay --profile release-replay
```

The single-account golden regression lives separately:

```sh
cargo test -p sweat_jar --features replay-engine replay_account_history
```
