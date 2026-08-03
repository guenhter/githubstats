#!/usr/bin/env bash
# collect_month.sh
#
# Collects raw data for the previous calendar month:
#   1. GH Archive events  → data/archive-YYYYMM.csv
#   2. GitHub languages   → data/languages-YYYY-MM.jsonl
#
# Intended use: cron on the 1st of each month.
#   0 2 1 * * /path/to/githubstats/collect_month.sh >> /path/to/githubstats/logs/collect.log 2>&1
#
# Requirements on the target machine: docker (no Rust toolchain needed).
# Compiled artefacts are written to ./target inside the repo, so subsequent
# runs reuse them without any extra setup.

set -euo pipefail

# ── resolve project root (directory this script lives in) ──────────────────
SCRIPT_DIR=$(dirname "$(realpath "$0")")
cd "$SCRIPT_DIR"

# ── load env (GITHUB_TOKEN, etc.) ──────────────────────────────────────────
if [[ -f .env ]]; then
  # shellcheck source=.env
  source .env
fi

# ── verify required environment variables ─────────────────────────────────
: "${GITHUB_TOKEN:?GITHUB_TOKEN is not set. Export it or add it to .env}"

# ── docker helper ─────────────────────────────────────────────────────────
# Runs `cargo run --release --bin <binary>` inside the official rust image.
# The entire repo is mounted read/write so cargo can write to target/ directly,
# caching compiled artefacts between runs without any named volumes.
# --interactive keeps stdin open for binaries that read from it (e.g. github_language_loader).
DOCKER_RUN=(
  docker run --rm --interactive
  --mount "type=bind,source=${SCRIPT_DIR},target=/app"
  --env "GITHUB_TOKEN=${GITHUB_TOKEN}"
  --workdir /app
  rust:1.96
  cargo run --release --bin
)

# ── derive previous month ──────────────────────────────────────────────────
YEAR=$(date -d "$(date +%Y-%m-01) -1 month" +%Y)
MONTH=$(date -d "$(date +%Y-%m-01) -1 month" +%m)   # zero-padded, e.g. 06

echo "[$(date -Iseconds)] collect_month.sh starting for ${YEAR}-${MONTH}"

# ── Step 1: GH Archive loader ──────────────────────────────────────────────
ARCHIVE_OUT="data/archive-${YEAR}${MONTH}.csv"

if [[ -f "$ARCHIVE_OUT" ]]; then
  echo "[$(date -Iseconds)] Step 1 skipped — $ARCHIVE_OUT already exists"
else
  echo "[$(date -Iseconds)] Step 1 — downloading GH Archive for ${YEAR}-${MONTH}"
  "${DOCKER_RUN[@]}" github_archive_loader -- \
    --year  "$YEAR"  \
    --month "$MONTH" \
    --parallelism 10 \
    --output "data/archive-${YEAR}${MONTH}.csv"
  echo "[$(date -Iseconds)] Step 1 done → $ARCHIVE_OUT"
fi

# ── Step 1b: filter (bot/noise removal + low-activity tail trim) ───────────
# Produces the filtered CSV that Step 2 reads from.  --repo-min-events 50
# drops the long tail of repos with fewer than 50 monthly events, which
# contribute negligibly to the ratings but dominate the language-loader's
# GraphQL fetch volume (the binding constraint under GitHub's secondary
# rate limit).
FILTERED_OUT="data/archive-${YEAR}${MONTH}-filtered.csv"

if [[ -f "$FILTERED_OUT" ]]; then
  echo "[$(date -Iseconds)] Step 1b skipped — $FILTERED_OUT already exists"
else
  echo "[$(date -Iseconds)] Step 1b — filtering $ARCHIVE_OUT"
  "${DOCKER_RUN[@]}" filter_archive -- \
    --input  "$ARCHIVE_OUT" \
    --output "$FILTERED_OUT" \
    --repo-min-events 50
  echo "[$(date -Iseconds)] Step 1b done → $FILTERED_OUT"
fi

# ── Step 2: GitHub language loader (resumable) ────────────────────────────
# If a previous run was interrupted, the partial languages-YYYY-MM.jsonl is
# kept and only the repos not yet present in it are fetched again; new results
# are appended. This makes Step 2 safe to interrupt and resume at any time.
LANGUAGES_OUT="data/languages-${YEAR}-${MONTH}.jsonl"
DONE_REPOS=$(mktemp)
PENDING=$(mktemp)

# Build the set of repos already present in the output file (empty if none yet).
if [[ -f "$LANGUAGES_OUT" ]]; then
  awk -F'"' '/"repo"/{print $4}' "$LANGUAGES_OUT" | sort -u > "$DONE_REPOS"
else
  : > "$DONE_REPOS"
fi

# Full slug list for the month, minus the already-done set → still pending.
# Reads from the *filtered* CSV so only repos that survived the filter chain
# (including the --repo-min-events 50 tail trim) are fetched.  This keeps the
# language-loader's GraphQL fetch volume within GitHub's rate-limit ceiling.
awk -F',' 'NR>1 && $3=="PushEvent" {print $2}' "$FILTERED_OUT" | sort -u \
  | comm -23 - "$DONE_REPOS" > "$PENDING"

PENDING_COUNT=$(wc -l < "$PENDING")
if [[ "$PENDING_COUNT" -eq 0 ]]; then
  echo "[$(date -Iseconds)] Step 2 skipped — $LANGUAGES_OUT already complete"
else
  echo "[$(date -Iseconds)] Step 2 — fetching $PENDING_COUNT repos for ${YEAR}-${MONTH}"
  # awk/sort run on the host; their output is piped into the container via stdin.
  # Inter-request pacing is handled internally by the loader (adaptive cooldown).
  cat "$PENDING" \
    | "${DOCKER_RUN[@]}" github_language_loader -- \
    >> "$LANGUAGES_OUT"
  echo "[$(date -Iseconds)] Step 2 done → $LANGUAGES_OUT"
fi

rm -f "$DONE_REPOS" "$PENDING"

echo "[$(date -Iseconds)] collect_month.sh finished for ${YEAR}-${MONTH}"
