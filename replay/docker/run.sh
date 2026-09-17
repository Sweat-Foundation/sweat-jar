#!/usr/bin/env bash
# Build the replay image (if needed) and run it against ./test_data on this
# machine. Run from the repo root, after test_data/interest_replay/ (and
# test_data/products.json, if you have one already) are in place — see
# replay/docker/README.md for how to get the data there.
#
# Anything after `--` is passed straight to the container's entrypoint,
# overriding the default build-db-then-run pipeline, e.g.:
#   ./replay/docker/run.sh -- explain --db /data/replay.duckdb --account 42 --archival
#
# Every REPLAY_* / FASTNEAR_API_KEY env var already set in your shell is
# forwarded to the container — see replay/docker/README.md for the full list
# (REPLAY_THREADS, REPLAY_SAMPLE, REPLAY_ACCOUNTS, REPLAY_FORCE, REPLAY_ARCHIVAL,
# REPLAY_CORRECTED_SCORE_WINDOW, REPLAY_TIMING, ...).
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

IMAGE="${REPLAY_IMAGE:-sweat-jar-replay:local}"
DATA_DIR="${REPLAY_DATA_DIR:-$(pwd)/test_data}"

if [ ! -d "$DATA_DIR/interest_replay" ]; then
    echo "error: $DATA_DIR/interest_replay not found." >&2
    echo "Copy the interest_replay/ parquet export there first (see replay/docker/README.md)." >&2
    exit 1
fi

echo "==> building $IMAGE"
docker build -f replay/Dockerfile -t "$IMAGE" .

# Forward every exported REPLAY_* / FASTNEAR_API_KEY var, so the container
# sees exactly what this shell has — nothing more, nothing baked in. (`env`,
# not `compgen -v`: only vars actually exported to child processes reach
# `docker run` at all.)
env_args=()
while IFS='=' read -r name _; do
    env_args+=(-e "$name")
done < <(env | grep -E '^(REPLAY_[A-Z_]*|FASTNEAR_API_KEY)=' || true)

extra_args=()
if [ "${1:-}" = "--" ]; then
    shift
    extra_args=("$@")
fi

# -t only if stdin is actually a terminal — under `set -eu` a plain `-it` in
# a non-interactive context (CI, a background job, a piped invocation) either
# hangs waiting for a TTY that will never appear or fails outright.
tty_args=()
[ -t 0 ] && tty_args=(-t)

echo "==> running $IMAGE (data: $DATA_DIR)"
# `${arr[@]+"${arr[@]}"}`, not `"${arr[@]}"`: macOS ships bash 3.2, where the
# latter is an unbound-variable error under `set -u` when the array is empty.
docker run --rm -i ${tty_args[@]+"${tty_args[@]}"} \
    -v "$DATA_DIR:/data" \
    ${env_args[@]+"${env_args[@]}"} \
    "$IMAGE" \
    ${extra_args[@]+"${extra_args[@]}"}
