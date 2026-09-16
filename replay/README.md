# `replay` — per-account yield reconciliation

Replays each account's **exhaustive on-chain event stream** through the local
contract engine and compares the **replayed total claim** against the
**on-chain total claim**, emitting one CSV row per account with the signed
delta. Population-scale counterpart of the single-account
`replay_account_history` golden test in `contract/src/replay.rs`.

## Window

| bound | value | meaning |
|-------|-------|---------|
| `H` (start) | `1774017710156` ms | block `190375496` — baseline state is "as of `H`" |
| `T_end` (end) | `1788174657961` ms | end of the replay window |

Defined in `replay/src/parse.rs` (`H_MS`, `H_BLOCK`, `T_END_MS`), written to
the `meta` table at build time.

## Input

`test_data/interest_replay/` — gitignored, ~25 GB, three parquet dirs:

| dir | columns | notes |
|-----|---------|-------|
| `events/` | `backend_account_id, block_timestamp_utc, log_index, receipt_status, event, role, payload` | `payload` is JSON. Only `receipt_status = 'SUCCESS_VALUE'` rows are ingested. Events: `record_score`, `claim`, `apply_booster` (role `applied`/`rejected`), `deposit`, `withdraw_all`, `restake`, `set_feature_enabled`. |
| `accounts/` | `backend_account_id, near_account_id, existed_at_start, created_in_window` | `existed_at_start` accounts need an archival baseline at block `H`. |
| `account_timezones/` | `backend_account_id, timezone_ms` | authoritative per-account timezone; `NULL` for a few that never set one. |

Optional fourth dir, `jars_merge_events/` (see [`db` schema](#db-schema)).

## Build

```sh
cargo build -p replay --profile release-replay
BIN=./target/release-replay/replay
```

The custom profile exists because `.cargo/config.toml` sets `panic = "abort"`
on plain `release` (for the contract wasm), which makes `catch_unwind` a
no-op. `[profile.release-replay]` (root `Cargo.toml`) is `release` +
`panic = "unwind"` + `debug-assertions = true` (the latter so `require!`
expands to an unwinding `assert!`, not a nounwind abort).

`replay` depends on `sweat_jar` with the `replay-engine` feature (pulls
`near-sdk/unit-testing`, host-only) and is not in the workspace
`default-members`, so plain `cargo build`/`cargo test`/CI are unaffected —
always build it explicitly with `-p replay`.

Add `--features corrected-score-window` to reconcile against "what should
have been paid" (current, fixed score-window logic) instead of "what was
actually paid" (default — reproduces the pre-v4.2.3 bug, see
[Known divergences](#known-divergences)). Rebuilds `target/release-replay/replay`
in place; only one mode is available per build.

DuckDB is vendored via the `duckdb` crate's `bundled` feature — no external
binary needed.

## Commands

### `build-db`

```
replay build-db --db x.duckdb --source test_data/interest_replay [--accounts file] [--sample N]
```

Builds a sorted `.duckdb` with `events`, `accounts`, `snapshots`, `meta` (and
`migrations` if `jars_merge_events/` is present). `--accounts <file>` keeps
only the listed `backend_account_id`s (one integer/line, `#` comments OK);
`--sample N` keeps only the first `N`. `--accounts` beats `--sample`.

### `run`

```
replay run --db x.duckdb [--out reconciliation.csv]
  [--threads N] [--archival] [--archival-rpc-url URL]
  [--shard i/n] [--accounts file] [--sample N] [--tolerance f] [--force]
```

Threaded per-account reconciliation, resumable by default. `--shard i/n`
processes only accounts where `backend_account_id.rem_euclid(n) == i`
(fan-out over one DB across processes/machines). `--tolerance` (default
`1e-6`): an `ok` row with `|rel_delta|` above it counts toward
`over_tolerance`.

`--archival` fetches **every** account's block-`H` state live from a NEAR
archival node (`get_account` at `H_BLOCK`) instead of the local `snapshots`
table — `existed_at_start` is just the export's own classification, not
trusted, so it no longer gates the fetch. A `null` response is authoritative
("no state at `H`"), not a gap — the account's first `Deposit` creates it,
same as the real contract. Without `--archival`, a missing `snapshots` row
for an `existed_at_start` account is marked `no_baseline` instead.

**`FASTNEAR_API_KEY`** (env only, never a flag/log) adds
`Authorization: Bearer <key>` to archival requests. Worth having — the free
tier throttles hard above ~4 concurrent requests, the main cap on
`--threads` for a full-population run.

```sh
export FASTNEAR_API_KEY=...   # keep out of shell history / committed files
replay run --db x.duckdb --archival --threads 16
```

**`REPLAY_TIMING=1`** prints one `TIMING account=<id> status=<s> load_us=<..>
fetch_us=<..> replay_us=<..> total_us=<..>` line per account to stderr, for
sizing a run before committing to it. `replay_us` scales ~linearly with
event count (~9ms/event on this crate's build — near-sdk mock overhead
dominates, not the interest math); `fetch_us` (archival RPC) is roughly flat
per account (~0.4s on FastNEAR). For a large population, replay CPU time can
rival or exceed the archival fetch.

**Progress.** `run` prints to stderr as it goes (all visible via
`docker logs -f` if running remotely, see `replay/docker/README.md`):

- `ERROR account=<id> error:<msg>` — the moment any account's status is
  `error:...`
- `FAILURE account=<id> failure:<msg>` — same, for `failure:...`
- `PROGRESS <done>/<total> (<pct>%) ok=<n> error=<n> failure=<n>
  no_baseline=<n> elapsed=<dur> rate=<n>/s eta=<dur>` — heartbeat every 30s
  (`REPLAY_PROGRESS_INTERVAL_SECS`, `0` disables), plus a final line at 100%.
  `<total>` excludes anything already resumed past, so a resumed run's % is
  progress on what's left.

You can also read the (already-written) `results` table directly from
another terminal at any time — DuckDB allows concurrent readers alongside
one writer — e.g. `duckdb x.duckdb "SELECT status, count(*) FROM results GROUP BY 1"`.

**Resumability.** Each `ReconRow` is upserted into `results` (keyed by
`backend_account_id`) as soon as computed, so a crash loses at most the row
in flight. The worklist is `accounts` minus what's already in `results`, so
re-running the same command after an interruption only computes what's
missing. `--force` recomputes everyone. `--out`, if given, is a full export
of `results` written at the end (not just this invocation's work) — omit it
to update only the DB and export later with `export-csv`.

### `export-csv`

```
replay export-csv --db x.duckdb --out reconciliation.csv
```

Writes `results` to CSV without recomputing anything. DuckDB locks the file
for exclusive access while `run` holds it open, so run this after `run`
exits (Ctrl-C included — already-committed rows are safe to export).

### `explain`

```
replay explain --db x.duckdb --account <backend_account_id> [--archival]
```

Per-claim breakdown (calculated vs on-chain) for one account, to trace a
non-zero `delta`.

### `bisect` (debug tool, separate binary)

```
cargo run -p replay --profile release-replay --bin bisect -- \
  --db x.duckdb --account <backend_account_id> [--archival-rpc-url URL]
```

Replays one account event-by-event and compares the in-memory state after
each event against a live archival `get_account` at that event's own block
(NEAR's view-at-height semantics guarantee that block already reflects the
event). Prints the first event where they diverge plus a JSON diff — how the
double-timezone-shift and rejected-booster bugs (below) were root-caused.
Always uses the archival RPC; there's no point bisecting against the local
`snapshots` cache.

### `fetch-products`

```
replay fetch-products
```

Refreshes `test_data/products.json` from mainnet.

## How the replay works

For each account:

1. **Baseline** — with `--archival`, `get_account` at block `H` for every
   account (`null` = confirmed empty); without it, the local `snapshots`
   table. If the account is invisibly pre-existing (see
   [`Account … is not found`](#known-error-causes) below), an
   `apply_block_height_fallback` step recovers it first.
2. **Timezone** — set from `account_timezones/` immediately before the
   account's first score-based jar is created (a deposit/restake into a
   `ScoreBased`/`TieredScoreBased` product), not upfront. No-op if there's
   no score jar or the baseline already carries a valid timezone.
3. **Replay** every event in `(block_timestamp_utc, log_index)` order,
   mapped to an engine `Action`:

   | event | `Action` |
   |-------|----------|
   | `record_score` | `RecordScore` |
   | `apply_booster` (both roles) | `ApplyBooster` |
   | `deposit` | `Deposit` |
   | `withdraw_all` | `WithdrawAll` |
   | `restake` | `Restake` / `RestakeAll` |
   | `set_feature_enabled` | `SetIncreasedScoreCap` |
   | `claim` | `Claim` |

   A same-block `deposit` + `apply_booster` pair (airdrop with booster)
   replays as one `Action::AirdropWithBooster`, mirroring the real
   contract's internal call order. `log_index` is authoritative intra-block
   order; a score/state/claim rank only breaks further ties between two
   distinct receipts sharing a timestamp and `log_index`.
4. Sum the `claim` payload `items` for the on-chain total.

## `reconciliation.csv`

```
account_id,near_account_id,calculated_total_claim,actual_total_claim,delta,rel_delta,n_claims,status
```

| column | meaning |
|--------|---------|
| `account_id` | = `backend_account_id` |
| `near_account_id` | on-chain `AccountId` |
| `calculated_total_claim` | Σ of amounts from replayed `claim`s |
| `actual_total_claim` | Σ of on-chain `claim` payload `items` |
| `delta` | `calculated − actual` (signed) |
| `rel_delta` | `delta / actual` (`0.0` when actual is `0`) |
| `n_claims` | claims replayed |
| `status` | `ok` \| `no_baseline` \| `error:<msg>` \| `failure:<msg>` |

`ok` — replayed from a real baseline. `no_baseline` — replayed from empty
state because the account pre-existed `H` and no archival baseline was
fetched (informational). `error:<msg>` and `failure:<msg>` both carry
`calculated = 0, delta = -actual`, but differ:

- **`error:<msg>`** — the fetch succeeded and the *contract logic itself*
  panicked on this account's real history (e.g. "Timezone is not set").
  **Not retryable** — deterministic. See
  [Known error causes](#known-error-causes).
- **`failure:<msg>`** — the tool didn't get a clean read (archival RPC
  error/throttle, or an unmodeled panic escaping the engine's guard — a bug,
  not contract behavior). **Worth retrying**: `export-csv`, filter
  `failure:` rows to an id list, `run --force --accounts <that file>` (after
  fixing connectivity / rotating the API key).

`run`'s stdout summary:

```
processed <n> | ok <n> | error <n> | failure <n> | no_baseline <n> | over_tolerance <n>
sum_calculated <n> | sum_actual <n>
```

## Known error causes

Causes you may still see — these are expected, not open bugs (the fixed
causes that used to dominate `error:`/`over_tolerance` rows — rejected
`apply_booster` discarding, the record_score timezone double-shift, the
claim timestamp, and airdrop-with-booster ordering — are already fixed in
the engine, so a fresh run shouldn't hit them):

- **`Timestamp from future`** — an export artifact: a `record_score`/
  `apply_booster` increment's own timestamp can trail its block time in the
  export even though the real call already passed this assertion on-chain.
  `timeline.rs` clamps each increment to `min(increment_ts, block_ts)`, so
  this doesn't surface.
- **`Account … is not found in smart contract`** — the account's jars were
  created by an `FtMessage::Migrate` from the pre-v2 contract, which writes
  storage directly (`store_account_raw`) and emits no v2-side event —
  invisible to all 7 tracked event types. `apply_block_height_fallback`
  resolves this, in order: (1) the `migrations` table (from
  `jars_merge_events/`, see [`db` schema](#db-schema)) — decode
  `raw_account` directly if present (~98% of rows, no RPC), else one
  archival fetch at the migration's own block; (2) if no `migrations` row
  exists, walk backward from the account's first tracked event up to
  `MAX_LOCK_WALKBACK_BLOCKS`, skipping any state with a locked jar
  (`Restake`/`Claim`/`Withdraw(All)` lock jars mid-flight, so the
  immediately-preceding block can be a transient snapshot of the *same*
  event). An unlocked state found this way replays the leading event
  normally; otherwise it falls back to the event's own post-state block and
  drops that leading event. Both paths need `--archival-rpc-url`; without
  one, such an account lands on `no_baseline`.
- **`Not enough funds to restake`** and small silent `over_tolerance`
  clusters after a multi-jar restake — `restake_all` is deterministic given
  accurate state (it recomputes the swept set from replay's own jar state,
  not an on-chain `from` list), so this isn't information loss: the
  replay's jar state at that instant can already be a hair off from
  on-chain (rounding, timezone estimation, an earlier timestamp clamp), and
  `restake_all`'s sharp matured-balance cutoff occasionally flips which
  jars count as matured, producing a one-time composition jump that
  persists through later claims. Rare (~0.1%–few % of accounts); accepted,
  not fixed.

## Known divergences

- The engine runs **current** contract code (window spans v4.1.0–4.2.2), so
  version-specific historical behavior isn't reproduced — **except** the
  dominant one: `AccountScore::shift()`/`wipe()` are `replay-engine`-
  conditional (`model/src/data/score/mod.rs`) to intentionally revert the
  v4.2.3 fix that stamps `score.updated_at` when `settle_interest` rolls the
  window, because the whole window predates that fix and every historical
  claim racing the oracle's daily `record_score` lost a day of accrual
  on-chain. The golden regression (`contract/src/replay/mod.rs`) pins a
  separate total per build config accordingly.
- `restake` with a multi-jar `from` set replays as `restake_all` with the
  exact `restaked` amount — the rest of the matured principal is withdrawn,
  which can differ from a genuine single-jar restake.
- `reconcile_user` wraps `run_timeline` in its own `catch_unwind` (the
  near-sdk unit-test mock can let a second panic escape the engine's
  internal guard); such accounts land on `failure:`, not `error:`.

## `db` schema

`replay/src/db/schema.rs`.

| table | key columns |
|-------|-------------|
| `events` | `backend_account_id, ts_ms, log_index, block_height, event, role, payload` |
| `accounts` | `backend_account_id, near_account_id, existed_at_start, timezone_ms` |
| `migrations` | `backend_account_id` (PK), `block_height`, `raw_account` — one row per account with a `JarsMerge` event on the old contract |
| `snapshots` | `backend_account_id, state_json` |
| `meta` | `key, value` |
| `results` | `run`'s output — `backend_account_id` (PK) + the `reconciliation.csv` columns + `computed_at` |

`migrations` is populated from an optional `jars_merge_events/` parquet dir
alongside `events/`/`accounts/`/`account_timezones/`; `build-db` skips it
silently if absent (falls back to the walk-back heuristic for every
account). Its `migrated_jars_borsh_base64` column is decoded straight to
`migrations.raw_account` at ingest via DuckDB's `from_base64` — no borsh
work outside the database.

## Testing

```sh
cargo test -p replay --profile release-replay
```

Single-account golden regression (separate):

```sh
cargo test -p sweat_jar --features replay-engine replay_account_history
```

## Running on another machine

`replay/docker/` has a `Dockerfile`, an entrypoint wrapping the whole
`build-db`-then-`run` pipeline behind env vars, and a `run.sh` to build and
launch it — for a long `--archival` run without tying up your own laptop.
Machine sizing (building needs ~8 GB RAM, the dataset is ~24 GB, a full run
without an API key takes about a week) and the full workflow are in
`replay/docker/README.md`.
