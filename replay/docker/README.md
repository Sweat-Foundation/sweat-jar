# Running `replay` on another machine (Docker)

For a long `--archival` run over the full account population without tying up
your own laptop. Everything here assumes Docker is installed on the target
machine; nothing else is.

## 1. Get the repo and the data there

```sh
# on the target machine
git clone git@github.com:Sweat-Foundation/sweat-jar.git
cd sweat-jar
git checkout replay-reconciliation   # or whichever branch/tag you're running

mkdir -p test_data
```

`test_data/interest_replay/` (the parquet export, ~24 GB) is gitignored and
never committed — copy it over separately, from wherever you already have it
(your machine, cloud storage, …):

```sh
# from your machine, to the target machine
rsync -avP test_data/interest_replay/ target-host:sweat-jar/test_data/interest_replay/
```

`test_data/products.json` is optional — the container fetches it from mainnet
on first run if it's missing (see step 3).

## 2. Build the image

```sh
./replay/docker/run.sh
```

The first invocation builds the image (`sweat-jar-replay:local`) if it isn't
already built — DuckDB compiles from source, so expect several minutes.
Rebuilding after a code change is the same command; Docker's layer cache
keeps it fast unless the source actually changed.

## 3. Run it

```sh
FASTNEAR_API_KEY=... REPLAY_THREADS=16 ./replay/docker/run.sh
```

With no extra arguments this runs the default pipeline: `build-db` once (only
if `test_data/replay.duckdb` doesn't exist yet), then `run --archival`. Both
read/write under the bind-mounted `test_data/` directory on the host, so the
database and the CSV are right there when the container exits — nothing is
trapped inside the container.

**It's resumable.** `run` skips accounts already in the `results` table by
default, so if the container is killed (or you `docker stop` it) and you run
the exact same command again, it picks up where it left off. No special
handling needed.

**Long runs**: this is a normal foreground container. Use `screen`/`tmux`, or
run it detached and follow the logs:

```sh
docker run --rm -d --name replay -v "$(pwd)/test_data:/data" \
    -e FASTNEAR_API_KEY -e REPLAY_THREADS=16 sweat-jar-replay:local
docker logs -f replay
```

`docker logs -f` is also how you track execution and errors while it's
running: `run` prints an `ERROR account=<id> ...` line to stderr the moment
any account fails, and a `PROGRESS <done>/<total> (<pct>%) ok=.. error=..
elapsed=.. eta=..` heartbeat every 30s (`REPLAY_PROGRESS_INTERVAL_SECS` to
change the interval, `0` to silence it) — see `replay/README.md` for the
exact line formats. You can also pull a point-in-time snapshot of `results`
at any time without stopping the run — DuckDB allows concurrent readers
alongside the one writer (the image has no `duckdb` CLI, but `export-csv` is
the same thing):

```sh
./replay/docker/run.sh -- export-csv --db /data/replay.duckdb --out /data/progress-snapshot.csv
```

### Environment variables

All optional; sensible defaults assume the whole population, unthrottled by
FastNEAR's free tier only if you don't set a key.

| Variable | Default | Meaning |
|---|---|---|
| `REPLAY_SOURCE` | `/data/interest_replay` | parquet export dir |
| `REPLAY_DB` | `/data/replay.duckdb` | DuckDB file (built once, reused) |
| `REPLAY_OUT` | `/data/reconciliation.csv` | CSV exported when `run` finishes |
| `REPLAY_PRODUCTS` | `/data/products.json` | product catalogue (auto-fetched if missing) |
| `REPLAY_THREADS` | all cores | worker threads for `run` |
| `REPLAY_SAMPLE` | — | `build-db`/`run --sample N` |
| `REPLAY_ACCOUNTS` | — | path (inside `/data`) to a `build-db`/`run --accounts` id list |
| `REPLAY_SHARD` | — | `run --shard i/n`, for splitting across multiple machines/containers |
| `REPLAY_ARCHIVAL` | `1` | set to `0` to read the local `snapshots` table instead of live archival lookups |
| `REPLAY_ARCHIVAL_RPC_URL` | FastNEAR public endpoint | override the archival RPC |
| `REPLAY_TOLERANCE` | `1e-6` | `run --tolerance` |
| `REPLAY_FORCE` | `0` | set to `1` to recompute accounts already in `results` |
| `REPLAY_REBUILD_DB` | `0` | set to `1` to re-run `build-db` even if `REPLAY_DB` exists |
| `REPLAY_CORRECTED_SCORE_WINDOW` | `0` | set to `1` to reconcile against "what should have been paid" instead of "what was actually paid" — see the main `replay/README.md` |
| `REPLAY_TIMING` | — | set to `1` for the per-account load breakdown (see `replay/README.md`) |
| `REPLAY_PROGRESS_INTERVAL_SECS` | `30` | seconds between `PROGRESS` heartbeat lines on stderr; `0` disables |
| `FASTNEAR_API_KEY` | — | archival RPC auth; strongly recommended for a full run — the free tier throttles hard above ~4 concurrent requests |

### Sharding across multiple machines

`REPLAY_SHARD=i/n` on `n` machines (or `n` containers on one machine) splits
the account population by `backend_account_id % n == i`, each writing into its
own `results` table region of a **shared** `.duckdb` file — DuckDB allows one
writer at a time, so either point each shard at its own DB file and merge the
`results` tables afterward with `export-csv`-style `COPY`, or run shards
sequentially against one file.

## 4. Get the results back

```sh
# on the target machine, any time (even mid-run, from another terminal —
# DuckDB allows concurrent readers alongside the one writer):
./replay/docker/run.sh -- export-csv --db /data/replay.duckdb --out /data/reconciliation.csv

# back on your machine
rsync -avP target-host:sweat-jar/test_data/reconciliation.csv .
# or just the database, to keep querying/exporting locally:
rsync -avP target-host:sweat-jar/test_data/replay.duckdb .
```

## Tracing one account

```sh
./replay/docker/run.sh -- explain --db /data/replay.duckdb --account <backend_account_id> --archival
```
