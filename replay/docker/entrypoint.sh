#!/usr/bin/env bash
# Entrypoint for the replay Docker image.
#
# With arguments: exec'd straight through to the selected binary — full
# control, e.g.:
#   docker run … explain --db /data/x.duckdb --account 42 --archival
#
# With no arguments: runs the default pipeline (build-db once, then run),
# configured entirely from environment variables — see replay/docker/README.md
# for the full list.
set -euo pipefail

BIN=/usr/local/bin/replay
if [ "${REPLAY_CORRECTED_SCORE_WINDOW:-0}" = "1" ]; then
    BIN=/usr/local/bin/replay-corrected-score-window
fi

if [ "$#" -gt 0 ]; then
    exec "$BIN" "$@"
fi

SOURCE="${REPLAY_SOURCE:-/data/interest_replay}"
DB="${REPLAY_DB:-/data/replay.duckdb}"
OUT="${REPLAY_OUT:-/data/reconciliation.csv}"
PRODUCTS="${REPLAY_PRODUCTS:-/data/products.json}"
THREADS="${REPLAY_THREADS:-$(nproc)}"

if [ ! -f "$PRODUCTS" ]; then
    echo "==> $PRODUCTS missing, fetching from mainnet"
    "$BIN" fetch-products --out "$PRODUCTS"
fi

build_db_args=(--db "$DB" --source "$SOURCE")
[ -n "${REPLAY_SAMPLE:-}" ] && build_db_args+=(--sample "$REPLAY_SAMPLE")
[ -n "${REPLAY_ACCOUNTS:-}" ] && build_db_args+=(--accounts "$REPLAY_ACCOUNTS")
[ -n "${REPLAY_MEMORY_LIMIT:-}" ] && build_db_args+=(--memory-limit "$REPLAY_MEMORY_LIMIT")

if [ ! -f "$DB" ] || [ "${REPLAY_REBUILD_DB:-0}" = "1" ]; then
    echo "==> build-db ${build_db_args[*]}"
    "$BIN" build-db "${build_db_args[@]}"
else
    echo "==> $DB already exists, skipping build-db (set REPLAY_REBUILD_DB=1 to force)"
fi

run_args=(--db "$DB" --out "$OUT" --products "$PRODUCTS" --threads "$THREADS")
[ "${REPLAY_ARCHIVAL:-1}" = "1" ] && run_args+=(--archival)
[ -n "${REPLAY_ARCHIVAL_RPC_URL:-}" ] && run_args+=(--archival-rpc-url "$REPLAY_ARCHIVAL_RPC_URL")
[ -n "${REPLAY_ACCOUNTS:-}" ] && run_args+=(--accounts "$REPLAY_ACCOUNTS")
[ -n "${REPLAY_SAMPLE:-}" ] && run_args+=(--sample "$REPLAY_SAMPLE")
[ -n "${REPLAY_SHARD:-}" ] && run_args+=(--shard "$REPLAY_SHARD")
[ -n "${REPLAY_TOLERANCE:-}" ] && run_args+=(--tolerance "$REPLAY_TOLERANCE")
[ "${REPLAY_FORCE:-0}" = "1" ] && run_args+=(--force)

echo "==> run ${run_args[*]}"
exec "$BIN" run "${run_args[@]}"
