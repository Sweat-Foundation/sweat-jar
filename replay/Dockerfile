# Build context is the repo root (`docker build -f replay/Dockerfile .`) —
# the workspace needs every member crate's source (primitives/, model/,
# contract/, replay/), not just replay/.
#
# Builds BOTH replay binaries: the default (reproduces the pre-v4.2.3
# score-window bug, matching real on-chain history) and
# `--features corrected-score-window` (what should have been paid). See
# replay/README.md's "Known divergences" section.

FROM rust:1.93-bookworm AS builder

# cmake + g++ are needed to compile DuckDB from source (the `duckdb` crate's
# `bundled` feature) — there is no prebuilt libduckdb dependency otherwise.
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    g++ \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace
COPY . .

# DuckDB's C++ unity-build translation units are large enough that compiling
# many of them in parallel can exhaust memory on a machine with lots of cores
# but modest RAM (e.g. a Docker Desktop VM) — cap the job count rather than
# defaulting to one-per-core. Bump this (or drop the flag) on a build host
# with more RAM per core.
RUN cargo build -p replay --profile release-replay --jobs 4 \
    && cp target/release-replay/replay /usr/local/bin/replay
RUN cargo build -p replay --profile release-replay --features corrected-score-window --jobs 4 \
    && cp target/release-replay/replay /usr/local/bin/replay-corrected-score-window

FROM debian:bookworm-slim AS runtime

# libstdc++6/libgcc-s1: the statically-built DuckDB is C++ code linked into
# the Rust binary; it still needs the C++ runtime shared libs at load time.
RUN apt-get update && apt-get install -y --no-install-recommends \
    libstdc++6 \
    libgcc-s1 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 1000 replay

COPY --from=builder /usr/local/bin/replay /usr/local/bin/replay
COPY --from=builder /usr/local/bin/replay-corrected-score-window /usr/local/bin/replay-corrected-score-window
COPY replay/docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

# All input (test_data/interest_replay/) and output (the .duckdb, the CSV)
# live under this bind-mounted directory — see replay/docker/README.md.
WORKDIR /data
VOLUME ["/data"]
USER replay

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
