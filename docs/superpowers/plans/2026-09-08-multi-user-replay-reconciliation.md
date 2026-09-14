# Multi-user Replay Reconciliation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Re-simulate every Sweat user's jar activity over a fixed window against the local contract, backed by SQLite, and emit a per-user report comparing replayed total claimed vs on-chain total claimed.

**Architecture:** The simulation core is extracted from `contract/src/replay.rs` into a `contract`-crate module (`replay::engine`) gated behind a new `replay-engine` cargo feature, so both the existing single-account test and a new binary crate use one implementation with no dependency cycle. A new workspace member `replay/` (binary crate) ingests the four `test_data/*.csv` files into SQLite (`build-db`), then streams per-user slices, synthesizes an event timeline, runs `engine::run_timeline`, and writes `reconciliation.csv` (`run`), parallelised across named worker threads.

**Tech Stack:** Rust, `rusqlite` (bundled SQLite), `csv`, `clap` (derive), `serde_json`, `ureq` (blocking HTTP for the products RPC), `anyhow`, `near-sdk` test utils (via the `replay-engine` feature).

**Spec:** `docs/superpowers/specs/2026-09-08-multi-user-replay-reconciliation-design.md`

## Global Constraints

- Window bounds, verbatim: `H = 1774017710156` (block 190375496, 2026-03-20T14:41:50.156Z), `T_end = 1788174657961` (2026-08-31T11:10:57.961Z). Events kept iff `H < ts_ms <= T_end`. (Verify with `python3 -c "import datetime as d; print(int(d.datetime(2026,3,20,14,41,50,156000,tzinfo=d.timezone.utc).timestamp()*1000))"`.)
- `account_id` (integer) is the join key across all four CSVs (`users.account_id == jar_events.account_id == step_packages.account_id == max_subscriptions.user_id`). `near_account_id` (64-hex string) is the on-chain `AccountId`. `sweatcoin_user_id` is NOT ingested.
- Amounts are stored as decimal strings and parsed to `u128` (yocto, the SWEAT base unit = `10^18`); never parse token amounts as `f64`.
- `merge` jar-events are dropped. `deposit_ids`, `fee_amount`, `product_name` columns are dropped.
- `subscriptions` toggle `Feature::IncreasedScoreCap` only. `Feature::IncreasedApy` comes from the baseline snapshot and never changes during a replay.
- The `replay/` crate is added to `[workspace].members` but NOT `default-members`, so `cargo build` / `cargo test` at the repo root and in CI are unchanged.
- Deposits are submitted unsigned; engine products carry `public_key: None` (matches current `replay.rs`).
- Every worker thread MUST be given a name (`std::thread::Builder::name`). `contract`'s test-env storage (`common::env::test_env_ext`) keys data by `std::thread::current().name().unwrap()` and panics on an unnamed thread.

---

## File Structure

**Modified in `contract/`:**
- `contract/Cargo.toml` — add `[features] replay-engine`.
- `contract/src/lib.rs` — gate `mod replay` on `any(test, feature = "replay-engine")`; expose `pub mod replay` under the feature.
- `contract/src/common/mod.rs`, `common/testing.rs`, `common/env.rs`, `common/event.rs`, `common/assertions.rs` — widen `#[cfg(test)]` gates to `#[cfg(any(test, feature = "replay-engine"))]` where the engine needs the symbol.
- `contract/src/migration/api.rs` — widen the gate on `store_account_raw` (currently `pub(crate)`, file not test-gated but confirm it compiles under the feature).
- `contract/src/replay.rs` → becomes a thin wrapper over `replay::engine`.

**Created in `contract/`:**
- `contract/src/replay/mod.rs` — `pub mod engine;` plus shared re-exports.
- `contract/src/replay/engine.rs` — `Timeline`, `Event`, `Action`, `AccountState`, `ReplayOutcome`, `ReplayStatus`, `run_timeline`.
- `contract/src/replay/engine_tests.rs` — unit tests for `run_timeline` on synthetic timelines.

**Created — new crate `replay/`:**
- `replay/Cargo.toml`
- `replay/src/main.rs` — `clap` CLI dispatch.
- `replay/src/cli.rs` — arg structs.
- `replay/src/db/mod.rs` — connection helpers, `OpenFlags`, pragmas.
- `replay/src/db/schema.rs` — `CREATE TABLE` / `CREATE INDEX` SQL + `init_schema`.
- `replay/src/db/ingest.rs` — `build_db`: per-table CSV → SQLite loaders.
- `replay/src/parse.rs` — timestamp / amount parsers (shared with tests).
- `replay/src/products.rs` — `fetch_products` (RPC) + `load_products` (file → `Vec<Product>`).
- `replay/src/snapshot.rs` — `SnapshotSource` trait, `FileSnapshotSource`, `ArchivalRpcSnapshotSource` (stub).
- `replay/src/timeline.rs` — per-user slice → `engine::Timeline`.
- `replay/src/reconcile.rs` — `reconcile_user` → `ReconRow`; CSV row shape.
- `replay/src/run.rs` — threaded driver, channel CSV writer, summary.
- `replay/tests/fixtures/` — tiny CSV samples.
- `replay/tests/build_db.rs`, `replay/tests/end_to_end.rs`, `replay/tests/thread_isolation.rs`.
- `replay/README.md`

---

## Task 1: `replay-engine` feature compiles the engine's dependencies

**Files:**
- Modify: `contract/Cargo.toml:24-31`
- Modify: `contract/src/lib.rs:19-20`
- Modify: `contract/src/common/mod.rs`
- Modify: `contract/src/common/testing.rs:1`
- Modify: `contract/src/common/env.rs` (the `test_env_ext` module gate)
- Modify: `contract/src/common/event.rs:210-219` (the `#[cfg(test)]` `emit`)
- Modify: `contract/src/common/assertions.rs:45`

**Interfaces:**
- Produces: cargo feature `replay-engine` on the `sweat_jar` crate. Under it, `sweat_jar::common::testing::Context`, `sweat_jar::common::env::test_env_ext`, and `sweat_jar::migration::api::store_account_raw` are compiled and reachable (visibility widened to `pub` in a later step only as needed).

- [ ] **Step 1: Add the feature**

In `contract/Cargo.toml` under `[features]`:

```toml
replay-engine = []
```

- [ ] **Step 2: Introduce a gate alias and widen the cfg gates**

Replace `#![cfg(test)]` at the top of `contract/src/common/testing.rs` with:

```rust
#![cfg(any(test, feature = "replay-engine"))]
```

In `contract/src/common/env.rs`, change the `test_env_ext` module attribute:

```rust
#[cfg(any(test, feature = "replay-engine"))]
pub(crate) mod test_env_ext {
```

In `contract/src/common/event.rs`, change the two `emit` cfg gates so the store-events variant is active under the feature too:

```rust
#[cfg(not(any(test, feature = "replay-engine")))]
pub(crate) fn emit(event: EventKind) {
    log!("{}", SweatJarEvent::from(event).to_json_event_string());
}

#[cfg(any(test, feature = "replay-engine"))]
pub(crate) fn emit(event: EventKind) {
    test_env_ext::store_event(&event);
    if test_env_ext::should_log_events() {
        log!("{}", SweatJarEvent::from(event).to_json_event_string());
    }
}
```

In `contract/src/common/assertions.rs:45`, widen the `#[cfg(test)]` to `#[cfg(any(test, feature = "replay-engine"))]` if that block is referenced by `testing.rs`; otherwise leave it.

In `contract/src/common/mod.rs`, widen the `testing` / `env` module visibility if needed so the feature build sees them (they are already `pub(crate)`; keep that).

- [ ] **Step 3: Gate the `replay` module in lib.rs**

In `contract/src/lib.rs` replace:

```rust
#[cfg(test)]
mod replay;
```

with:

```rust
#[cfg(any(test, feature = "replay-engine"))]
pub mod replay;
```

- [ ] **Step 4: Verify the feature build compiles**

Run: `cargo build -p sweat_jar --features replay-engine 2>&1 | tail -30`
Expected: compiles. Fix every `cfg(test)`-gated symbol the compiler reports as missing by widening its gate to `any(test, feature = "replay-engine")`. Do NOT widen gates that aren't reported — keep the surface minimal.

- [ ] **Step 5: Verify the normal test build is unchanged**

Run: `cargo test -p sweat_jar --lib 2>&1 | tail -20`
Expected: same pass/fail as before this task (all green).

- [ ] **Step 6: Verify the plain release build is unaffected**

Run: `cargo build -p sweat_jar 2>&1 | tail -5`
Expected: compiles, no new warnings about unused `replay` module.

- [ ] **Step 7: Commit**

```bash
git add contract/Cargo.toml contract/src/lib.rs contract/src/common contract/src/migration
git commit -m "feat(contract): add replay-engine feature gating test env for reuse"
```

---

## Task 2: Extract the replay engine

**Files:**
- Create: `contract/src/replay/mod.rs`
- Create: `contract/src/replay/engine.rs`
- Create: `contract/src/replay/engine_tests.rs`
- Modify: `contract/src/replay.rs` → move to `contract/src/replay/scenario.rs` (see Task 3); for this task, leave `replay.rs` in place and add the sibling module dir. Rust allows `replay.rs` + `replay/` to coexist only as `replay.rs` being the module file with submodules in `replay/`. So: rename `contract/src/replay.rs` to `contract/src/replay/mod.rs` first, then add `engine.rs` beside it.

**Interfaces:**
- Produces:
  ```rust
  // contract/src/replay/engine.rs
  pub use sweat_jar_model::{Score, UTC};
  pub use sweat_jar_model::data::product::Product;

  #[derive(Clone, Debug)]
  pub enum Action {
      RecordScore(Score),
      Deposit { product_id: String, amount: u128 },
      Withdraw { product_id: String },
      Restake { product_id: String },
      SetIncreasedScoreCap(bool),
      Claim,
  }

  #[derive(Clone, Debug)]
  pub struct Event { pub ts_ms: u64, pub seq: u64, pub action: Action }

  impl Action { pub fn rank(&self) -> u8 { /* RecordScore=0; Deposit|Withdraw|Restake|SetIncreasedScoreCap=1; Claim=2 */ } }

  #[derive(Default)]
  pub struct Timeline { pub events: Vec<Event> }
  impl Timeline {
      /// Sorts events by (ts_ms, rank, seq) in place.
      pub fn sorted(mut self) -> Self { /* ... */ }
  }

  /// Opaque baseline: the raw borsh bytes of an `AccountVersioned`, or None for a fresh account.
  pub struct Baseline {
      pub account_id: near_sdk::AccountId,
      pub raw_account: Option<Vec<u8>>,
  }

  #[derive(Debug)]
  pub enum ReplayStatus { Ok, Error(String) }

  #[derive(Debug)]
  pub struct ReplayOutcome {
      pub total_claimed: u128,
      pub per_claim: Vec<(u64, u128)>,
      pub status: ReplayStatus,
  }

  /// Runs the timeline against a fresh in-process contract on the current thread.
  /// Resets thread-local mock storage before starting. Never panics: contract
  /// panics are caught and surfaced as `ReplayStatus::Error`.
  pub fn run_timeline(baseline: Baseline, products: &[Product], window_start_ms: u64, timeline: Timeline) -> ReplayOutcome;
  ```
- Consumes (from Task 1): `crate::common::testing::Context`, `crate::common::env::test_env_ext`, `crate::migration::api::store_account_raw`.

- [ ] **Step 1: Rename the module file**

```bash
git mv contract/src/replay.rs contract/src/replay/mod.rs
mkdir -p contract/src/replay
```

Add to the top of `contract/src/replay/mod.rs`:

```rust
pub mod engine;
#[cfg(test)]
mod engine_tests;
```

Run: `cargo test -p sweat_jar --lib replay 2>&1 | tail -10` — expected: still compiles and the existing `replay_account_history` test still runs (it will fail to find `engine` until step 2 — that's fine, add an empty `engine.rs` now: `touch contract/src/replay/engine.rs`).

- [ ] **Step 2: Write the failing engine unit test**

`contract/src/replay/engine_tests.rs`:

```rust
use sweat_jar_model::{
    data::product::{Apy, Cap, FixedProductTerms, Product, Terms},
    Score,
};
use sweat_jar_primitives::UDecimal;

use super::engine::{run_timeline, Action, Baseline, Event, ReplayStatus, Timeline};

fn fixed_product() -> Product {
    Product {
        id: "365d_12apy".to_string(),
        cap: Cap::new(1_000_000_000_000_000_000, 500_000 * 10u128.pow(24)),
        terms: Terms::Fixed(FixedProductTerms {
            lockup_term: 31_536_000_000u64.into(),
            apy: Apy::Constant(UDecimal::new(12, 2)),
        }),
        withdrawal_fee: None,
        public_key: None,
        is_enabled: true,
    }
}

#[test]
fn deposit_then_claim_after_a_year_yields_roughly_apy() {
    let account_id: near_sdk::AccountId = "acc.near".parse().unwrap();
    let one_year_ms = 31_536_000_000u64;
    let deposit_amount = 1_000 * 10u128.pow(18);

    let timeline = Timeline {
        events: vec![
            Event { ts_ms: 10, seq: 0, action: Action::Deposit { product_id: "365d_12apy".into(), amount: deposit_amount } },
            Event { ts_ms: one_year_ms + 100, seq: 1, action: Action::Claim },
        ],
    }
    .sorted();

    let outcome = run_timeline(
        Baseline { account_id, raw_account: None },
        &[fixed_product()],
        0,
        timeline,
    );

    assert!(matches!(outcome.status, ReplayStatus::Ok));
    assert_eq!(outcome.per_claim.len(), 1);
    // 12% of 1000 SWEAT, within 1%.
    let expected = 120 * 10u128.pow(18);
    let claimed = outcome.total_claimed;
    assert!(claimed.abs_diff(expected) < expected / 100, "claimed {claimed} vs {expected}");
}
```

Run: `cargo test -p sweat_jar --lib engine_tests 2>&1 | tail -10`
Expected: FAIL — `run_timeline` not found.

- [ ] **Step 3: Implement the engine**

`contract/src/replay/engine.rs` — port the loop from the old `replay_account_history` body. Key parts:

```rust
use std::panic::{catch_unwind, AssertUnwindSafe};

use near_sdk::{json_types::Base64VecU8, AccountId, PromiseOrValue};
use sweat_jar_model::{
    api::{ClaimApi, RestakeApi, WithdrawApi, AccountApi},
    data::{
        account::features::Feature,
        deposit::DepositTicket,
        product::Product,
    },
    Score, UTC,
};

use crate::{
    common::{env::test_env_ext, testing::Context},
    migration::api::store_account_raw,
};

// ... Action / Event / Timeline / Baseline / ReplayOutcome as in the Interfaces block ...

pub fn run_timeline(baseline: Baseline, products: &[Product], window_start_ms: u64, timeline: Timeline) -> ReplayOutcome {
    test_env_ext::set_test_log_events(false);

    let account_id = baseline.account_id.clone();
    let result = catch_unwind(AssertUnwindSafe(|| {
        let mut context = Context::new(admin()).with_products(products);

        if let Some(raw) = &baseline.raw_account {
            store_account_raw(account_id.clone(), Base64VecU8(raw.clone()));
        }
        context.set_block_timestamp_in_ms(window_start_ms);

        let mut total_claimed = 0u128;
        let mut per_claim = Vec::new();

        for event in timeline.events {
            context.set_block_timestamp_in_ms(event.ts_ms);
            match event.action {
                Action::RecordScore(score) => {
                    context.switch_account_to_operator();
                    context.contract().record_score(vec![(account_id.clone(), vec![(score, UTC(event.ts_ms))])]);
                }
                Action::Deposit { product_id, amount } => {
                    let ticket = DepositTicket { product_id, valid_until: 0.into(), timezone: None };
                    context.switch_account_to_ft_contract_account();
                    context.contract().deposit(account_id.clone(), ticket, amount, None);
                }
                Action::Withdraw { product_id } => {
                    context.switch_account(&account_id);
                    let _ = context.contract().withdraw(product_id);
                }
                Action::Restake { product_id } => {
                    context.switch_account(&account_id);
                    let ticket = DepositTicket { product_id: product_id.clone(), valid_until: 0.into(), timezone: None };
                    let _ = context.contract().restake(product_id, ticket, None, None);
                }
                Action::SetIncreasedScoreCap(enabled) => {
                    context.switch_account_to_operator();
                    context.contract().set_feature_enabled(account_id.clone(), Feature::IncreasedScoreCap, enabled);
                }
                Action::Claim => {
                    context.switch_account(&account_id);
                    if let PromiseOrValue::Value(claimed) = context.contract().claim_total(None) {
                        let amount = claimed.get_total().0;
                        total_claimed += amount;
                        per_claim.push((event.ts_ms, amount));
                    }
                }
            }
        }
        (total_claimed, per_claim)
    }));

    match result {
        Ok((total_claimed, per_claim)) => ReplayOutcome { total_claimed, per_claim, status: ReplayStatus::Ok },
        Err(e) => {
            let msg = e.downcast_ref::<&str>().map(|s| s.to_string())
                .or_else(|| e.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            ReplayOutcome { total_claimed: 0, per_claim: Vec::new(), status: ReplayStatus::Error(msg) }
        }
    }
}

fn admin() -> AccountId { "admin.near".parse().unwrap() }
```

Notes for the implementer:
- `Context::new` already calls `near_sdk::mock::with_mocked_blockchain(|b| b.take_storage())`, so the storage reset is covered by constructing a fresh `Context` per call.
- `Context::deposit` is `Contract::deposit` (a `pub(crate)` helper) — it must be reachable from `crate::replay::engine`; it is, both are in `sweat_jar`.
- `context.contract()` returns a `MutexGuard<Contract>`; drop it between calls (each `context.contract()...` is its own statement, so the guard drops at the `;`).
- `withdraw` in the current contract withdraws the entire liquid principal of the product's jar; there is no partial-amount form. `jar_events.amount` for withdraw is informational. Document this as a known divergence for historical partial withdrawals (see README, Task 16).

- [ ] **Step 4: Run the engine test**

Run: `cargo test -p sweat_jar --lib engine_tests 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Add a panic-safety test**

Append to `engine_tests.rs`:

```rust
#[test]
fn claim_with_no_jars_is_reported_not_panicked() {
    let account_id: near_sdk::AccountId = "empty.near".parse().unwrap();
    let timeline = Timeline { events: vec![Event { ts_ms: 5, seq: 0, action: Action::Claim }] }.sorted();
    let outcome = run_timeline(Baseline { account_id, raw_account: None }, &[fixed_product()], 0, timeline);
    // Either Ok with zero claims, or Error — never a process panic.
    match outcome.status {
        ReplayStatus::Ok => assert_eq!(outcome.total_claimed, 0),
        ReplayStatus::Error(_) => {}
    }
}
```

Run: `cargo test -p sweat_jar --lib engine_tests 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add contract/src/replay
git commit -m "feat(contract): extract reusable replay engine"
```

---

## Task 3: Rewire the single-account scenario onto the engine

**Files:**
- Modify: `contract/src/replay/mod.rs` (the former `replay.rs` body)

**Interfaces:**
- Consumes: `engine::{Action, Baseline, Event, Timeline, run_timeline, ReplayStatus}`.

- [ ] **Step 1: Keep the golden assertion, swap the loop**

In `contract/src/replay/mod.rs`, keep `parse_snapshot`, `parse_interactions`, `parse_steps`, `catalogue`, and all parsing helpers. Replace the body of `replay_account_history` after event collection with:

```rust
let mut events: Vec<engine::Event> = Vec::new();
let mut seq = 0u64;
// map the file-parsed Action into engine::Action, assigning seq in file order
for e in parsed_events {
    events.push(engine::Event { ts_ms: e.ts, seq, action: e.action.into_engine() });
    seq += 1;
}
let timeline = engine::Timeline { events }
    .sorted();
// keep the window filter BEFORE sorting
let outcome = engine::run_timeline(
    engine::Baseline { account_id: snapshot.account_id.clone(), raw_account: Some(to_vec(&AccountVersioned::new(snapshot.account)).unwrap()) },
    &catalogue(),
    snapshot.block_time_ms,
    timeline,
);
for (ts, amount) in &outcome.per_claim {
    println!("{ts} - {amount}");
}
println!("---");
println!("claims: {}", outcome.per_claim.len());
println!("TOTAL CLAIMED: {}", outcome.total_claimed);
assert!(matches!(outcome.status, engine::ReplayStatus::Ok));
assert!(outcome.total_claimed > 0, "expected the account to have claimed something");
```

Provide an `impl Action { fn into_engine(self) -> engine::Action }` bridge in `mod.rs` for the local `Action` enum (the file parser can keep its own `Action` or be changed to build `engine::Action` directly — implementer's choice; the simpler path is to delete the local `Action` and have `parse_*` build `engine::Action`).

- [ ] **Step 2: Pin the golden total**

Run: `cargo test -p sweat_jar --lib replay_account_history -- --nocapture 2>&1 | tail -15`
Record the printed `TOTAL CLAIMED` value. Add:

```rust
const GOLDEN_TOTAL_CLAIMED: u128 = /* the printed value */;
assert_eq!(outcome.total_claimed, GOLDEN_TOTAL_CLAIMED, "golden replay total changed");
```

- [ ] **Step 3: Re-run to confirm the pin holds**

Run: `cargo test -p sweat_jar --lib replay_account_history 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add contract/src/replay/mod.rs
git commit -m "refactor(contract): run single-account replay through the engine"
```

---

## Task 4: Scaffold the `replay/` crate

**Files:**
- Create: `replay/Cargo.toml`
- Create: `replay/src/main.rs`
- Create: `replay/src/cli.rs`
- Modify: `Cargo.toml` (root) — `members`

**Interfaces:**
- Produces: a `replay` binary with subcommands `fetch-products`, `build-db`, `run` (arg parsing only; each prints `unimplemented` and exits 1).
  ```rust
  // replay/src/cli.rs
  #[derive(clap::Parser)]
  pub struct Cli { #[command(subcommand)] pub cmd: Cmd }
  #[derive(clap::Subcommand)]
  pub enum Cmd {
      FetchProducts { #[arg(long, default_value = "test_data/products.json")] out: PathBuf },
      BuildDb {
          #[arg(long)] db: PathBuf,
          #[arg(long, default_value = "test_data")] test_data_dir: PathBuf,
          #[arg(long, value_delimiter = ',')] only: Vec<String>,
          #[arg(long)] accounts: Option<PathBuf>,
          #[arg(long)] sample: Option<usize>,
      },
      Run {
          #[arg(long)] db: PathBuf,
          #[arg(long, default_value = "reconciliation.csv")] out: PathBuf,
          #[arg(long, default_value = "test_data/products.json")] products: PathBuf,
          #[arg(long)] threads: Option<usize>,
          #[arg(long)] shard: Option<String>,   // "I/N"
          #[arg(long)] accounts: Option<PathBuf>,
          #[arg(long)] sample: Option<usize>,
          #[arg(long, default_value_t = 1e-6)] tolerance: f64,
      },
  }
  ```

- [ ] **Step 1: Add the crate to the workspace**

Root `Cargo.toml`:

```toml
members = ["primitives", "model", "contract", "replay"]
```

(`default-members` stays `["contract"]`.)

- [ ] **Step 2: Write `replay/Cargo.toml`**

```toml
[package]
name = "replay"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
sweat_jar = { path = "../contract", features = ["replay-engine"] }
sweat-jar-model = { workspace = true }
sweat-jar-primitives = { workspace = true }
near-sdk = { workspace = true }
rusqlite = { version = "0.32", features = ["bundled"] }
csv = "1.3"
clap = { version = "4.5", features = ["derive"] }
serde = { workspace = true, features = ["derive"] }
serde_json = "1.0"
anyhow = { workspace = true }
ureq = "2.10"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Write `replay/src/main.rs`**

```rust
mod cli;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    match cli.cmd {
        cli::Cmd::FetchProducts { .. } => anyhow::bail!("unimplemented: fetch-products"),
        cli::Cmd::BuildDb { .. } => anyhow::bail!("unimplemented: build-db"),
        cli::Cmd::Run { .. } => anyhow::bail!("unimplemented: run"),
    }
}
```

- [ ] **Step 4: Verify it builds and parses**

Run: `cargo run -p replay -- --help 2>&1 | tail -20`
Expected: prints subcommand help.
Run: `cargo run -p replay -- build-db --db /tmp/x.db 2>&1 | tail -3`
Expected: exits non-zero with `unimplemented: build-db`.

- [ ] **Step 5: Confirm the root build is unaffected**

Run: `cargo build 2>&1 | tail -5`
Expected: builds `contract` only (default-members), no `replay` in the default set.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml replay/Cargo.toml replay/src
git commit -m "feat(replay): scaffold replay binary crate"
```

---

## Task 5: `parse.rs` — timestamp and amount parsers

**Files:**
- Create: `replay/src/parse.rs`
- Modify: `replay/src/main.rs` (`mod parse;`)
- Test: `replay/src/parse.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  ```rust
  /// "2026-03-20T14:41:54.953Z" -> 1774017714953
  pub fn iso8601_ms_to_epoch_ms(s: &str) -> anyhow::Result<u64>;
  /// "2026-03-21 19:30:53 UTC" -> epoch ms
  pub fn space_utc_to_epoch_ms(s: &str) -> anyhow::Result<u64>;
  /// decimal string of yocto -> u128 (no decimal point expected; trims whitespace)
  pub fn yocto_str_to_u128(s: &str) -> anyhow::Result<u128>;
  pub const H_MS: u64 = 1_774_017_710_156;
  pub const T_END_MS: u64 = 1_788_174_657_961;
  ```

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn iso_ms() {
        assert_eq!(iso8601_ms_to_epoch_ms("2026-03-20T14:41:54.953Z").unwrap(), 1_774_017_714_953);
        assert_eq!(iso8601_ms_to_epoch_ms("2025-12-19T08:42:06.000Z").unwrap(), 1_766_133_726_000);
    }
    #[test]
    fn space_utc() {
        assert_eq!(space_utc_to_epoch_ms("2026-03-21 19:30:53 UTC").unwrap(), 1_774_121_453_000);
    }
    #[test]
    fn yocto() {
        assert_eq!(yocto_str_to_u128(" 500000000000000000000000 ").unwrap(), 500_000_000_000_000_000_000_000);
        assert!(yocto_str_to_u128("12.5").is_err());
    }
}
```

Compute the expected epoch values with an independent tool before writing them (e.g. `python3 -c "import datetime; print(int(datetime.datetime(2026,3,20,14,41,54,953000,tzinfo=datetime.timezone.utc).timestamp()*1000))"`).

Run: `cargo test -p replay parse 2>&1 | tail -10` — Expected: FAIL (module missing).

- [ ] **Step 2: Implement**

Port `parse_utc_datetime` / `days_from_civil` from `contract/src/replay/mod.rs` (Howard Hinnant's algorithm, already in the repo). Add the millisecond fraction handling for the ISO form and the ` UTC` suffix stripping for the space form. `yocto_str_to_u128` = `s.trim().parse::<u128>().map_err(...)`.

Run: `cargo test -p replay parse 2>&1 | tail -10` — Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add replay/src/parse.rs replay/src/main.rs
git commit -m "feat(replay): timestamp and amount parsers"
```

---

## Task 6: DB schema + connection helpers

**Files:**
- Create: `replay/src/db/mod.rs`, `replay/src/db/schema.rs`
- Modify: `replay/src/main.rs` (`mod db;`)
- Test: `replay/tests/build_db.rs`

**Interfaces:**
- Produces:
  ```rust
  // replay/src/db/mod.rs
  pub fn open_write(path: &Path) -> anyhow::Result<rusqlite::Connection>;   // sets build pragmas
  pub fn open_read(path: &Path) -> anyhow::Result<rusqlite::Connection>;    // SQLITE_OPEN_READ_ONLY
  // replay/src/db/schema.rs
  pub fn init_schema(conn: &rusqlite::Connection) -> anyhow::Result<()>;    // CREATE TABLE IF NOT EXISTS x5 + meta
  pub fn create_indexes(conn: &rusqlite::Connection) -> anyhow::Result<()>;
  ```

- [ ] **Step 1: Write the failing test**

`replay/tests/build_db.rs`:

```rust
use replay::db;  // requires exposing a lib target — see step 2

#[test]
fn schema_creates_expected_tables() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let conn = db::open_write(&path).unwrap();
    db::schema::init_schema(&conn).unwrap();
    let mut names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").unwrap()
        .query_map([], |r| r.get(0)).unwrap()
        .collect::<Result<_,_>>().unwrap();
    names.retain(|n| !n.starts_with("sqlite_"));
    assert_eq!(names, vec!["jar_events","meta","snapshots","step_packages","subscriptions","users"]);
}
```

- [ ] **Step 2: Add a lib target to the crate**

`replay/Cargo.toml`:

```toml
[lib]
name = "replay"
path = "src/lib.rs"

[[bin]]
name = "replay"
path = "src/main.rs"
```

Create `replay/src/lib.rs` re-exporting the modules (`pub mod db; pub mod parse; pub mod products; pub mod snapshot; pub mod timeline; pub mod reconcile; pub mod run; pub mod cli;`). `main.rs` becomes `use replay::cli;` etc.

Run: `cargo test -p replay schema_creates 2>&1 | tail -10` — Expected: FAIL (no `init_schema`).

- [ ] **Step 3: Implement schema.rs**

Exact DDL from the spec's "SQLite database → Schema" section (six `CREATE TABLE`, three `CREATE INDEX`). `open_write` runs `PRAGMA synchronous=OFF; PRAGMA journal_mode=MEMORY;`. `open_read` uses `Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)`.

Run: `cargo test -p replay schema_creates 2>&1 | tail -10` — Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add replay/Cargo.toml replay/src/lib.rs replay/src/main.rs replay/src/db replay/tests/build_db.rs
git commit -m "feat(replay): sqlite schema and connection helpers"
```

---

## Task 7: Ingest `users` and `subscriptions`

**Files:**
- Create: `replay/src/db/ingest.rs`
- Create: `replay/tests/fixtures/users.csv`, `replay/tests/fixtures/max_subscriptions.csv`
- Modify: `replay/src/db/mod.rs` (`pub mod ingest;`), `replay/src/main.rs` (wire `BuildDb`)
- Test: `replay/tests/build_db.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct BuildOpts<'a> {
      pub test_data_dir: &'a Path,
      pub only: &'a [String],          // empty = all tables
      pub accounts: Option<&'a HashSet<i64>>,
      pub sample: Option<usize>,
  }
  pub fn build_db(conn: &mut rusqlite::Connection, opts: &BuildOpts) -> anyhow::Result<()>;
  fn ingest_users(conn, opts) -> anyhow::Result<HashSet<i64>>;   // returns the account_id set actually ingested
  fn ingest_subscriptions(conn, keep: &HashSet<i64>, dir) -> anyhow::Result<()>;
  ```
- The `keep` set: when `accounts` is given, `keep = accounts`; else when `sample = N`, `keep` = first N `account_id`s from `users.csv`; else `keep` = all (represented as `None`, meaning "no filter").

- [ ] **Step 1: Fixtures**

`replay/tests/fixtures/users.csv`:

```
account_id,near_account_id,sweatcoin_user_id
4,0c1570c257cfa088aa60153d8d46a783a3b493a392863ece53cb47c0c4a2c992,14.0
36988193,9b6b8403e3ccbd6ba868c58d3e583939f62ca3abc3b2fa22f72b5030cd04efa8,135561109.0
```

`replay/tests/fixtures/max_subscriptions.csv`:

```
user_id,datetime,action_type
4,2025-12-19T08:42:06.000Z,subscribed
4,2026-12-19T23:59:59.999Z,expired
36988193,2026-04-01T00:00:00.000Z,subscribed
```

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn ingest_users_and_subscriptions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let mut conn = db::open_write(&path).unwrap();
    db::schema::init_schema(&conn).unwrap();
    db::ingest::build_db(&mut conn, &db::ingest::BuildOpts {
        test_data_dir: std::path::Path::new("tests/fixtures"),
        only: &["users".into(), "subscriptions".into()],
        accounts: None, sample: None,
    }).unwrap();

    let users: i64 = conn.query_row("SELECT count(*) FROM users", [], |r| r.get(0)).unwrap();
    assert_eq!(users, 2);
    let near: String = conn.query_row("SELECT near_account_id FROM users WHERE account_id=4", [], |r| r.get(0)).unwrap();
    assert!(near.starts_with("0c1570"));
    let subs: Vec<(i64, i64, i64)> = conn
        .prepare("SELECT account_id, ts_ms, active FROM subscriptions ORDER BY ts_ms").unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap()
        .collect::<Result<_,_>>().unwrap();
    assert_eq!(subs[0], (4, 1_766_133_726_000, 1));  // subscribed
    assert_eq!(subs.iter().filter(|(_,_,a)| *a == 0).count(), 1);  // one 'expired'
}
```

Run: `cargo test -p replay ingest_users_and_subscriptions 2>&1 | tail -10` — Expected: FAIL.

- [ ] **Step 3: Implement**

`ingest_users`: `csv::Reader` over `users.csv`, insert `(account_id, near_account_id)` only; ignore `sweatcoin_user_id`. Respect `sample` / `accounts`. Wrap in one transaction.
`ingest_subscriptions`: read `max_subscriptions.csv`; `user_id` → `account_id`; `datetime` via `parse::iso8601_ms_to_epoch_ms`; `subscribed` → `active = 1`, `expired` → `0`. Skip rows whose `account_id` is not in `keep` (when a filter is active). One transaction.
`build_db` dispatches by `only` (empty = all), calls `create_indexes` at the end unless `only` excludes everything relevant.

Run: `cargo test -p replay ingest_users_and_subscriptions 2>&1 | tail -10` — Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add replay/src/db/ingest.rs replay/src/db/mod.rs replay/src/main.rs replay/tests
git commit -m "feat(replay): ingest users and subscriptions"
```

---

## Task 8: Ingest `jar_events`

**Files:**
- Modify: `replay/src/db/ingest.rs`
- Create: `replay/tests/fixtures/jar_events.csv`
- Test: `replay/tests/build_db.rs`

**Interfaces:**
- Produces: `fn ingest_jar_events(conn, keep: Option<&HashSet<i64>>, dir) -> anyhow::Result<()>`
- Row rules: header `account_id,jar_id,product_id,product_name,near_block_timestamp,event_type,amount,fee_amount,deposit_ids`. Keep `event_type` in `{deposit, claim, withdraw, restake}` (drop `merge`). `ts_ms = iso8601_ms_to_epoch_ms(near_block_timestamp)`; keep iff `H_MS < ts_ms <= T_END_MS`. `seq` = 0-based line index within the file (monotonic, used as the final tie-breaker). `amount` stored as the raw string.

- [ ] **Step 1: Fixture**

`replay/tests/fixtures/jar_events.csv` (timestamps chosen to straddle the window; include a `merge` and a pre-`H` row that must both be dropped):

```
account_id,jar_id,product_id,product_name,near_block_timestamp,event_type,amount,fee_amount,deposit_ids
36988193,1,365d_12apy,The Rockstar,2026-03-20T14:41:50.100Z,deposit,1000000000000000000,0,1
36988193,1,365d_12apy,The Rockstar,2026-03-20T14:42:00.000Z,deposit,15760000000000000000,0,2
36988193,2,steps_365d_20000_score_cap,The Stepper,2026-04-01T00:00:00.000Z,merge,0,0,3 4
36988193,1,365d_12apy,The Rockstar,2026-06-14T15:04:10.853Z,claim,81505017066108257,0,2
36988193,1,365d_12apy,The Rockstar,2026-09-01T00:00:00.000Z,claim,999,0,2
```

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn ingest_jar_events_filters_window_and_merge() {
    // build users + jar_events from fixtures ...
    let rows: Vec<(i64,String,String,i64)> = conn
        .prepare("SELECT account_id,event_type,amount,ts_ms FROM jar_events ORDER BY ts_ms,seq").unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap()
        .collect::<Result<_,_>>().unwrap();
    // pre-H deposit dropped, merge dropped, post-T_end claim dropped
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "deposit");
    assert_eq!(rows[1].1, "claim");
    assert_eq!(rows[1].2, "81505017066108257");
}
```

Run: `cargo test -p replay ingest_jar_events 2>&1 | tail -10` — Expected: FAIL.

- [ ] **Step 3: Implement + Step 4: Run (PASS) + Step 5: Commit**

```bash
git add replay/src/db/ingest.rs replay/tests
git commit -m "feat(replay): ingest jar_events with window and merge filtering"
```

---

## Task 9: Ingest `step_packages` and `snapshots`; finalize `build-db` flags

**Files:**
- Modify: `replay/src/db/ingest.rs`, `replay/src/main.rs`
- Create: `replay/tests/fixtures/step_packages.csv`, `replay/tests/fixtures/snapshots.ndjson`
- Test: `replay/tests/build_db.rs`

**Interfaces:**
- Produces:
  ```rust
  fn ingest_step_packages(conn, keep: Option<&HashSet<i64>>, dir) -> anyhow::Result<()>;
  fn ingest_snapshots(conn, keep: Option<&HashSet<i64>>, dir) -> anyhow::Result<()>; // no-op if snapshots.ndjson absent
  ```
- `step_packages.csv`: `account_id,created_at,steps`. `ts_ms = space_utc_to_epoch_ms(created_at)`; keep iff in window; `steps` clamped to `65535`. Insert with a prepared statement inside a single transaction; commit every 1_000_000 rows to bound the WAL/journal. This is the ~285M-row table.
- `snapshots.ndjson`: one JSON object per line; the object has a `near_account_id` field and matches the `account_state` + `products_referenced` shape of `test_data/account_full_state_190375496.json`. Store `(account_id, raw_json_line)` — resolve `account_id` by joining `near_account_id` against the `users` table (so `users` must be ingested first when `snapshots` is in `only`).

- [ ] **Step 1: Fixtures**

`step_packages.csv`:

```
account_id,created_at,steps
36988193,2026-03-20 14:41:50 UTC,500
36988193,2026-03-21 19:30:53 UTC,99999
36988193,2026-09-05 00:00:00 UTC,10
```

`snapshots.ndjson` (single line; reuse the shape of the real file, trimmed to one jar):

```
{"near_account_id":"9b6b8403e3ccbd6ba868c58d3e583939f62ca3abc3b2fa22f72b5030cd04efa8","account_state":{"nonce":1,"jars":{},"score":{"updated_at":1774017710156,"history":[{"value":0,"booster":0},{"value":0,"booster":0}]},"features":{"increased_score_cap":false,"increased_apy":true},"timezone":0},"products_referenced":[]}
```

- [ ] **Step 2: Write failing tests**

```rust
#[test]
fn ingest_step_packages_clamps_and_windows() {
    // ... build users + step_packages ...
    let rows: Vec<(i64,i64)> = conn.prepare("SELECT ts_ms,steps FROM step_packages ORDER BY ts_ms").unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<Result<_,_>>().unwrap();
    assert_eq!(rows.len(), 2);       // pre-H (14:41:50 == H rounded down to sec => <= H) dropped
    assert_eq!(rows[0].1, 65535);    // clamped
}

#[test]
fn ingest_snapshots_joins_on_near_account_id() {
    // ... build users then snapshots ...
    let cnt: i64 = conn.query_row("SELECT count(*) FROM snapshots WHERE account_id=36988193", [], |r| r.get(0)).unwrap();
    assert_eq!(cnt, 1);
}
```

Note on the pre-`H` boundary: `2026-03-20 14:41:50 UTC` → `1774017710000` which is `< H_MS` (1774017710156), so it is correctly dropped by the `H_MS < ts_ms` rule.

Run: `cargo test -p replay 'ingest_step_packages|ingest_snapshots' 2>&1 | tail -12` — Expected: FAIL.

- [ ] **Step 3: Implement**

Add both loaders. Wire `build_db` so `only` accepts `users,jar_events,step_packages,subscriptions,snapshots`; empty `only` = all five. Compute `keep` once in `build_db` (accounts-file → parse newline-separated ints; else sample → first N from `users.csv`; else `None`). Call `create_indexes` after loads.

In `main.rs`, implement the `BuildDb` arm: open/create the DB, `init_schema`, `build_db(&mut conn, &opts)`, print row counts per table.

Run: `cargo test -p replay build_db 2>&1 | tail -15` — Expected: all PASS.

- [ ] **Step 4: Smoke-test against real data (sampled)**

Run: `cargo run -p replay -- build-db --db /tmp/replay-sample.db --sample 200 2>&1 | tail -10`
Expected: completes in seconds; prints non-zero counts for `users`, `jar_events`, `step_packages`.

- [ ] **Step 5: Commit**

```bash
git add replay/src/db/ingest.rs replay/src/main.rs replay/tests
git commit -m "feat(replay): ingest step_packages and snapshots; finalize build-db"
```

---

## Task 10: `products.rs` — fetch and load the product catalogue

**Files:**
- Create: `replay/src/products.rs`
- Modify: `replay/src/lib.rs`, `replay/src/main.rs` (wire `FetchProducts`)
- Create: `replay/tests/fixtures/products.json`
- Test: `replay/src/products.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  ```rust
  /// Calls get_products() on `contract` via RPC and writes pretty JSON to `out`.
  pub fn fetch_products(rpc_url: &str, contract: &str, out: &Path) -> anyhow::Result<()>;
  /// Parses the file written by fetch_products into engine products.
  pub fn load_products(path: &Path) -> anyhow::Result<Vec<sweat_jar_model::data::product::Product>>;
  pub const MAINNET_RPC: &str = "https://rpc.mainnet.near.org";
  pub const JAR_CONTRACT: &str = "v2.jars.sweat";
  ```
- RPC request body: `{"jsonrpc":"2.0","id":"1","method":"query","params":{"request_type":"call_function","finality":"final","account_id":"<contract>","method_name":"get_products","args_base64":"e30="}}`. Response: `result.result` is a byte array; `String::from_utf8` it, then it is a JSON array of products in the exact shape `Product` deserializes from (verified: the live payload matches `sweat_jar_model::data::product::Product`'s serde repr — `terms: {type, data}`, `cap: [min, max]` as strings, `apy: {default: [sig, exp]}`).

- [ ] **Step 1: Save a real fixture**

```bash
curl -s -X POST https://rpc.mainnet.near.org -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":"1","method":"query","params":{"request_type":"call_function","finality":"final","account_id":"v2.jars.sweat","method_name":"get_products","args_base64":"e30="}}' \
  | python3 -c "import json,sys; d=json.load(sys.stdin); sys.stdout.write(bytes(d['result']['result']).decode())" \
  | python3 -m json.tool > replay/tests/fixtures/products.json
```

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn load_products_parses_the_live_shape() {
    let products = load_products(std::path::Path::new("tests/fixtures/products.json")).unwrap();
    assert!(products.iter().any(|p| p.id == "365d_12apy"));
    let p = products.iter().find(|p| p.id == "365d_12apy").unwrap();
    assert!(matches!(p.terms, sweat_jar_model::data::product::Terms::Fixed(_)));
    assert!(p.public_key.is_none() || p.public_key.is_some()); // just assert it deserialized
}
```

Run: `cargo test -p replay load_products 2>&1 | tail -10` — Expected: FAIL.

- [ ] **Step 3: Implement**

`load_products`: `serde_json::from_reader::<Vec<Product>>`. If `Product`'s serde repr does NOT match the file (field name / tag mismatch), add a local `RawProduct` mirror struct + `impl From<RawProduct> for Product` rather than changing the model crate. `fetch_products`: `ureq::post(rpc_url).send_json(body)`, extract `result.result` as `Vec<u8>`, decode, re-serialize pretty to `out`.

Run: `cargo test -p replay load_products 2>&1 | tail -10` — Expected: PASS.

- [ ] **Step 4: Wire `FetchProducts` and smoke test**

`main.rs` `FetchProducts { out }` → `products::fetch_products(products::MAINNET_RPC, products::JAR_CONTRACT, &out)`.
Run: `cargo run -p replay -- fetch-products --out /tmp/products.json && head -c 200 /tmp/products.json`
Expected: writes a JSON array.

- [ ] **Step 5: Commit the fixture and the committed catalogue**

```bash
cargo run -p replay -- fetch-products --out test_data/products.json
git add replay/src/products.rs replay/src/lib.rs replay/src/main.rs replay/tests/fixtures/products.json test_data/products.json
git commit -m "feat(replay): fetch and load product catalogue"
```

---

## Task 11: `snapshot.rs` — baseline source

**Files:**
- Create: `replay/src/snapshot.rs`
- Modify: `replay/src/lib.rs`
- Test: `replay/src/snapshot.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  ```rust
  pub struct AccountState { /* mirrors account_full_state_190375496.json's account_state */ }
  pub trait SnapshotSource: Send + Sync {
      /// Returns borsh bytes of an `AccountVersioned` for this account at H, or None.
      fn raw_account(&self, account_id: i64) -> anyhow::Result<Option<Vec<u8>>>;
  }
  /// Reads the `snapshots` table.
  pub struct DbSnapshotSource { /* holds the db path, opens per-call read conn */ }
  /// Unimplemented stub for the future archival-RPC path.
  pub struct ArchivalRpcSnapshotSource { pub rpc_url: String, pub block_height: u64 }
  impl SnapshotSource for ArchivalRpcSnapshotSource {
      fn raw_account(&self, _: i64) -> anyhow::Result<Option<Vec<u8>>> { anyhow::bail!("archival-rpc snapshot source not implemented") }
  }
  /// JSON (account_state shape) -> borsh(AccountVersioned) bytes. Sets score.updated_at to H if zero.
  pub fn account_state_json_to_raw(json: &str) -> anyhow::Result<Vec<u8>>;
  ```
- `account_state_json_to_raw` reuses the parsing logic already in `contract/src/replay/mod.rs::parse_snapshot` — extract that into a `pub fn parse_account_state(json: &serde_json::Value) -> Account` in `contract/src/replay/engine.rs` (behind the feature) and call it here.

- [ ] **Step 1: Extract `parse_account_state` into the engine**

Move the jar/score/features parsing out of `parse_snapshot` into `engine::parse_account_state(&Value) -> sweat_jar_model::data::account::Account`. Update `contract/src/replay/mod.rs::parse_snapshot` to call it. Re-run `cargo test -p sweat_jar --lib replay_account_history` — Expected: PASS (golden unchanged).

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn json_snapshot_round_trips_to_borsh() {
    let json = std::fs::read_to_string("tests/fixtures/snapshots.ndjson").unwrap();
    let line = json.lines().next().unwrap();
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    let raw = super::account_state_json_to_raw(&v["account_state"].to_string()).unwrap();
    assert!(!raw.is_empty());
    // deserializes back as AccountVersioned
    use near_sdk::borsh::BorshDeserialize;
    sweat_jar_model::data::account::versioned::AccountVersioned::try_from_slice(&raw).unwrap();
}
```

Run: `cargo test -p replay json_snapshot_round_trips 2>&1 | tail -10` — Expected: FAIL.

- [ ] **Step 3: Implement + Step 4: Run (PASS)**

- [ ] **Step 5: Commit**

```bash
git add contract/src/replay replay/src/snapshot.rs replay/src/lib.rs
git commit -m "feat(replay): baseline snapshot source with archival-rpc stub"
```

---

## Task 12: `timeline.rs` — per-user slice → engine timeline

**Files:**
- Create: `replay/src/timeline.rs`
- Modify: `replay/src/lib.rs`
- Test: `replay/tests/build_db.rs` or a new `replay/tests/timeline.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct UserSlice {
      pub account_id: i64,
      pub near_account_id: String,
      pub onchain_claimed: u128,   // SUM(jar_events.amount) WHERE event_type='claim'
  }
  /// Reads all rows for one account from an open read connection, builds a sorted engine timeline.
  pub fn load_user(conn: &rusqlite::Connection, account_id: i64) -> anyhow::Result<(UserSlice, sweat_jar::replay::engine::Timeline)>;
  ```
- Synthesis rules (from the spec):
  - `jar_events` `deposit` → `Action::Deposit { product_id, amount: yocto_str_to_u128(amount) }`
  - `jar_events` `withdraw` → `Action::Withdraw { product_id }`
  - `jar_events` `restake` → `Action::Restake { product_id }`
  - `jar_events` `claim` → collapse to ONE `Action::Claim` per distinct `ts_ms` (drop per-jar duplication); still sum every claim row's `amount` into `onchain_claimed`.
  - `step_packages` → `Action::RecordScore(Score::try_from(steps).unwrap_or(Score::MAX))`
  - `subscriptions` → `Action::SetIncreasedScoreCap(active == 1)`
  - `Event.seq`: jar_events use their stored `seq`; scores use `1_000_000_000 + row_index`; subscriptions use `2_000_000_000 + row_index` (keeps a stable, source-segregated tie-break after `(ts_ms, rank)`).

- [ ] **Step 1: Write the failing test**

Build a small DB from fixtures (users + jar_events + step_packages + subscriptions for account 36988193), then:

```rust
#[test]
fn load_user_collapses_claims_and_sorts() {
    // ... build db ...
    let conn = db::open_read(&path).unwrap();
    let (slice, timeline) = replay::timeline::load_user(&conn, 36988193).unwrap();
    assert_eq!(slice.onchain_claimed, 81505017066108257);   // from the fixture claim row(s)
    // exactly one Claim action despite multiple claim rows at the same ts
    let claims = timeline.events.iter().filter(|e| matches!(e.action, sweat_jar::replay::engine::Action::Claim)).count();
    assert_eq!(claims, 1);
    // events sorted ascending by ts
    assert!(timeline.events.windows(2).all(|w| (w[0].ts_ms, w[0].action.rank()) <= (w[1].ts_ms, w[1].action.rank())));
}
```

Run: `cargo test -p replay load_user_collapses 2>&1 | tail -10` — Expected: FAIL.

- [ ] **Step 2: Implement + Step 3: Run (PASS) + Step 4: Commit**

```bash
git add replay/src/timeline.rs replay/src/lib.rs replay/tests
git commit -m "feat(replay): synthesize per-user engine timeline from db slice"
```

---

## Task 13: `reconcile.rs` — single-user reconciliation

**Files:**
- Create: `replay/src/reconcile.rs`
- Modify: `replay/src/lib.rs`
- Test: `replay/tests/end_to_end.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, serde::Serialize)]
  pub struct ReconRow {
      pub account_id: i64,
      pub near_account_id: String,
      pub calculated_total_claim: String,   // u128 as decimal string
      pub actual_total_claim: String,
      pub delta: String,                    // i128 as decimal string (calculated - actual)
      pub rel_delta: f64,                   // delta / actual, 0.0 when actual == 0
      pub n_claims: usize,
      pub status: String,                   // "ok" | "error:<msg>" | "no_baseline"
  }
  pub fn reconcile_user(
      conn: &rusqlite::Connection,
      account_id: i64,
      products: &[sweat_jar_model::data::product::Product],
      snapshot: &dyn crate::snapshot::SnapshotSource,
  ) -> anyhow::Result<ReconRow>;
  ```
- `status`: `no_baseline` when `snapshot.raw_account` returns `None` AND the user has `deposit`/`claim` events implying prior state (heuristic: any claim in the first hour after `H`); `error:<msg>` when `ReplayOutcome::status` is `Error`; else `ok`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn reconcile_user_produces_a_row() {
    // build a db from fixtures for account 36988193, load products from tests/fixtures/products.json,
    // use snapshot::DbSnapshotSource
    let row = replay::reconcile::reconcile_user(&conn, 36988193, &products, &snap).unwrap();
    assert_eq!(row.account_id, 36988193);
    assert_eq!(row.actual_total_claim, "81505017066108257");
    assert!(row.status == "ok" || row.status.starts_with("no_baseline"));
    // delta = calculated - actual, parses as i128
    row.delta.parse::<i128>().unwrap();
}
```

Run: `cargo test -p replay reconcile_user_produces 2>&1 | tail -10` — Expected: FAIL.

- [ ] **Step 2: Implement**

`reconcile_user` = `load_user` → `snapshot.raw_account` → `engine::Baseline` → `engine::run_timeline(baseline, products, parse::H_MS, timeline)` → assemble `ReconRow`. `delta` computed as `i128::try_from(calculated).unwrap() - i128::try_from(actual).unwrap()` (both fit: yocto totals for one user stay well under `i128::MAX`).

Run: `cargo test -p replay reconcile_user_produces 2>&1 | tail -10` — Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add replay/src/reconcile.rs replay/src/lib.rs replay/tests/end_to_end.rs
git commit -m "feat(replay): single-user reconciliation row"
```

---

## Task 14: `run.rs` — threaded driver, CSV writer, summary

**Files:**
- Create: `replay/src/run.rs`
- Modify: `replay/src/lib.rs`, `replay/src/main.rs` (wire `Run`)
- Test: `replay/tests/end_to_end.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct RunOpts {
      pub db: PathBuf, pub out: PathBuf, pub products: PathBuf,
      pub threads: usize, pub shard: Option<(u64, u64)>,   // (i, n)
      pub accounts: Option<PathBuf>, pub sample: Option<usize>,
      pub tolerance: f64,
  }
  pub fn run(opts: &RunOpts) -> anyhow::Result<RunSummary>;
  pub struct RunSummary {
      pub processed: usize, pub ok: usize, pub errored: usize, pub no_baseline: usize,
      pub sum_calculated: u128, pub sum_actual: u128,
      pub over_tolerance: usize,
  }
  ```
- Threading: build the account-id worklist with `SELECT account_id FROM users` (+ `WHERE account_id % n = i` for a shard, + `LIMIT sample`, + intersect with `accounts` file). Spawn `threads` workers via `std::thread::Builder::new().name(format!("replay-worker-{k}"))`. Each worker: opens its own `db::open_read`, loads products once (cheap clone of the shared `Arc<Vec<Product>>`), pulls account ids from a shared `Arc<Mutex<std::vec::IntoIter<i64>>>` (or an `mpsc` work channel), calls `reconcile_user`, sends `ReconRow` on an `mpsc::Sender<ReconRow>`. A dedicated writer thread owns the `csv::Writer` and drains the receiver. Join all; compute `RunSummary`; print it.
- The worker thread name is REQUIRED (global constraint) — `test_env_ext` panics otherwise.

- [ ] **Step 1: Write the failing end-to-end test**

```rust
#[test]
fn run_end_to_end_sample() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("e2e.db");
    // build a DB from tests/fixtures covering accounts 4 and 36988193
    let out = dir.path().join("rec.csv");
    let summary = replay::run::run(&replay::run::RunOpts {
        db: db.clone(), out: out.clone(), products: "tests/fixtures/products.json".into(),
        threads: 2, shard: None, accounts: None, sample: None, tolerance: 1e-6,
    }).unwrap();
    assert_eq!(summary.processed, 2);
    let body = std::fs::read_to_string(&out).unwrap();
    assert_eq!(body.lines().next().unwrap(),
        "account_id,near_account_id,calculated_total_claim,actual_total_claim,delta,rel_delta,n_claims,status");
    assert_eq!(body.lines().count(), 3); // header + 2 rows
}
```

Run: `cargo test -p replay run_end_to_end_sample 2>&1 | tail -12` — Expected: FAIL.

- [ ] **Step 2: Implement `run` + Step 3: Run (PASS)**

- [ ] **Step 4: Wire `main.rs` and smoke-test on sampled real data**

`Run { .. }` → parse `shard` `"i/n"` → `RunOpts` (`threads` default `std::thread::available_parallelism()`), call `run`, print summary, exit 0.

```bash
cargo run -p replay -- build-db --db /tmp/replay-sample.db --sample 500
cargo run -p replay -- run --db /tmp/replay-sample.db --out /tmp/rec.csv --threads 4
head -5 /tmp/rec.csv && wc -l /tmp/rec.csv
```

Expected: ~500 data rows, summary printed, no panic. (Most rows will be `no_baseline` until the real snapshot export lands — that is expected and documented.)

- [ ] **Step 5: Commit**

```bash
git add replay/src/run.rs replay/src/lib.rs replay/src/main.rs replay/tests/end_to_end.rs
git commit -m "feat(replay): threaded reconciliation driver and report"
```

---

## Task 15: Thread-isolation regression test

**Files:**
- Create: `replay/tests/thread_isolation.rs`

**Interfaces:**
- Consumes: `replay::run::run`, `replay::reconcile::reconcile_user`.

- [ ] **Step 1: Write the test**

Two users on the SAME worker thread, run back-to-back: user A has deposits + a claim; user B (fixture) has zero jar events. Assert B's `calculated_total_claim == "0"` and `status` is not `error`, proving A's mock storage did not leak into B.

```rust
#[test]
fn second_user_on_a_worker_is_not_polluted_by_the_first() {
    // build a db: account A (36988193) with deposit+claim fixtures, account B (4) with NO jar_events
    let summary = replay::run::run(&RunOpts { threads: 1, /* forces both users onto one worker */ .. }).unwrap();
    // read rec.csv, find row for account 4
    assert_eq!(row_b.calculated_total_claim, "0");
    assert!(!row_b.status.starts_with("error"));
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p replay second_user_on_a_worker 2>&1 | tail -10`
Expected: PASS. If it fails, the fix is in `engine::run_timeline` — ensure a fresh `Context::new` per call (it calls `blockchain.take_storage()`); do NOT reuse a `Context` across users.

- [ ] **Step 3: Commit**

```bash
git add replay/tests/thread_isolation.rs
git commit -m "test(replay): guard against mock-storage carryover between users"
```

---

## Task 16: Documentation

**Files:**
- Create: `replay/README.md`
- Modify: `test_data/REPLAY_DATA_SPEC.md` (add a pointer to the multi-user path)

**Interfaces:** none.

- [ ] **Step 1: Write `replay/README.md`**

Cover: purpose; the `fetch-products` → `build-db` → `run` workflow with real commands; the window constants; every `build-db` / `run` flag; the SQLite schema; how sharding + `--threads` compose; **known limitations** — (a) product config is current-state only, not as-of-window; (b) `withdraw` withdraws the full liquid principal (no partial form), so historical partial withdrawals diverge; (c) `merge` events are ignored; (d) users without a baseline snapshot replay from empty and surface as `no_baseline` / large negative delta until the archival-RPC extractor lands; (e) signatures are not verified.

- [ ] **Step 2: Cross-link the spec**

Add a short section to `test_data/REPLAY_DATA_SPEC.md` noting that `replay/` consumes the four full-population CSVs (`users.csv`, `jar_events.csv`, `step_packages.csv`, `max_subscriptions.csv`) and see `replay/README.md`.

- [ ] **Step 3: Commit**

```bash
git add replay/README.md test_data/REPLAY_DATA_SPEC.md
git commit -m "docs(replay): usage and known limitations"
```

---

## Self-Review

**Spec coverage:**
- Window/inputs/join key → Global Constraints + Tasks 5–9. ✓
- Products via `get_products()` cached to `test_data/products.json` → Task 10. ✓
- Baseline snapshot placeholder + archival-RPC stub → Tasks 11, 9 (ingest). ✓
- Crate layout, engine in `contract` behind feature, no cycle → Tasks 1–3. ✓
- SQLite schema + indexes-after-load + pragmas + `--only`/`--accounts`/`--sample` → Tasks 6–9. ✓
- Event synthesis table + `rank`/`seq` + claim-collapse + score-cap-only subscriptions → Task 12 + engine `Action::rank` (Task 2). ✓
- `withdraw`/`restake` through the real API → Task 2 engine dispatch. ✓
- Reconciliation math + `reconciliation.csv` columns + summary → Tasks 13–14. ✓
- Threading with named workers + `--shard` → Task 14 + Global Constraints. ✓
- Testing: parsers, merge/order, aggregation, golden, integration, thread-isolation → Tasks 5, 12, 13, 3, 14, 15. ✓
- Risks: storage carryover (Task 15), ingest cost (Task 9 batched commits + `--sample`), contract panics (Task 2 `catch_unwind`), product drift (Task 16 docs), no baseline (Task 13 `no_baseline` status). ✓

**Placeholder scan:** Task 8 Step 3 compresses "implement + run + commit" — the pattern is fully specified by the identical structure in Tasks 6–7 and the Interfaces block; acceptable. No `TODO`/`TBD`/"handle edge cases" left.

**Type consistency:** `engine::Action` / `Event` / `Timeline` / `Baseline` / `ReplayOutcome` / `ReplayStatus` defined in Task 2, consumed unchanged in Tasks 3, 12, 13. `ReconRow` defined in Task 13, consumed in Tasks 14–15. `BuildOpts` (Task 7) extended, not redefined, in Task 9. `SnapshotSource::raw_account(i64) -> Result<Option<Vec<u8>>>` consistent across Tasks 11, 13, 14.

---

## Execution Handoff

Two execution options:

1. **Subagent-Driven (recommended)** — a fresh subagent per task, review between tasks.
2. **Inline Execution** — tasks executed in this session with checkpoints.
