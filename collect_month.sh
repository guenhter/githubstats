#!/usr/bin/env bash
# collect_month.sh
#
# Collects and publishes stats for the previous calendar month:
#   1. GH Archive events     → data/archives/archive-YYYYMM.csv
#   2. Filter noise          → data/archives-filtered/archive-YYYYMM-filtered.csv
#   3. GitHub languages      → data/languages/languages-YYYY-MM.jsonl
#   4. Per-month ratings     → data/stats/language-ratings-YYYY-MM-<type>.jsonl
#   5. Pack all-months files → data/stats/language-ratings-all-<type>.jsonl
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

# ── derive previous month ──────────────────────────────────────────────────
YEAR=$(date -d "$(date +%Y-%m-01) -1 month" +%Y)
MONTH=$(date -d "$(date +%Y-%m-01) -1 month" +%m)   # zero-padded, e.g. 06
YM="${YEAR}-${MONTH}"
YYYYMM="${YEAR}${MONTH}"

STAT_TYPES=(pr-count issue-count push-count active-repos star-count)

# Stable per-step names so a crashed previous run can be cleaned up, and a
# second concurrent invocation for the same month fails fast on name conflict
# instead of spawning an anonymous duplicate.
docker_run() {
  local step=$1
  shift
  local name="githubstats-${step}-${YYYYMM}"

  # Drop a leftover container from an interrupted earlier run of this step.
  docker container rm -f "$name" >/dev/null 2>&1 || true

  docker container run \
    --name "$name" \
    --rm \
    --interactive \
    --mount "type=bind,source=${SCRIPT_DIR},target=/app" \
    --env "GITHUB_TOKEN=${GITHUB_TOKEN}" \
    --workdir /app \
    rust:1.96 \
    cargo run --release --bin "$@"
}

echo "[$(date -Iseconds)] collect_month.sh starting for ${YM}"

mkdir -p data/archives data/archives-filtered data/languages data/stats

# ── Step 1: GH Archive loader ──────────────────────────────────────────────
ARCHIVE_OUT="data/archives/archive-${YYYYMM}.csv"

if [[ -f "$ARCHIVE_OUT" ]]; then
  echo "[$(date -Iseconds)] Step 1 skipped — $ARCHIVE_OUT already exists"
else
  echo "[$(date -Iseconds)] Step 1 — downloading GH Archive for ${YM}"
  docker_run archive_loader github_archive_loader -- \
    --year  "$YEAR"  \
    --month "$MONTH" \
    --parallelism 10 \
    --output "$ARCHIVE_OUT"
  echo "[$(date -Iseconds)] Step 1 done → $ARCHIVE_OUT"
fi

# ── Step 2: filter (bot/noise removal + low-activity tail trim) ────────────
# Defaults match filter_archive / the historical stats series
# (--actor-event-limit 1000, --repo-push-limit 100, --repo-min-events 10).
FILTERED_OUT="data/archives-filtered/archive-${YYYYMM}-filtered.csv"

if [[ -f "$FILTERED_OUT" ]]; then
  echo "[$(date -Iseconds)] Step 2 skipped — $FILTERED_OUT already exists"
else
  echo "[$(date -Iseconds)] Step 2 — filtering $ARCHIVE_OUT"
  docker_run filter_archive filter_archive -- \
    --input  "$ARCHIVE_OUT" \
    --output "$FILTERED_OUT"
  echo "[$(date -Iseconds)] Step 2 done → $FILTERED_OUT"
fi

# ── Step 3: GitHub language loader (resumable) ─────────────────────────────
# If a previous run was interrupted, the partial languages-YYYY-MM.jsonl is
# kept and only the repos not yet present in it are fetched again; new results
# are appended. This makes Step 3 safe to interrupt and resume at any time.
LANGUAGES_OUT="data/languages/languages-${YM}.jsonl"
DONE_REPOS=$(mktemp)
PENDING=$(mktemp)
trap 'rm -f "$DONE_REPOS" "$PENDING"' EXIT

# Build the set of repos already present in the output file (empty if none yet).
if [[ -f "$LANGUAGES_OUT" ]]; then
  awk -F'"' '/"repo"/{print $4}' "$LANGUAGES_OUT" | sort -u > "$DONE_REPOS"
else
  : > "$DONE_REPOS"
fi

# Full slug list for the month, minus the already-done set → still pending.
# Reads from the *filtered* CSV so only repos that survived the filter chain
# are fetched.  This keeps the language-loader's GraphQL fetch volume within
# GitHub's rate-limit ceiling.
awk -F',' 'NR>1 && ($3 == "PullRequestEvent" || $3 == "IssuesEvent" || $3 == "PushEvent" || $3 == "WatchEvent") {print $2}' "$FILTERED_OUT" | sort -u \
  | comm -23 - "$DONE_REPOS" > "$PENDING"

PENDING_COUNT=$(wc -l < "$PENDING")
if [[ "$PENDING_COUNT" -eq 0 ]]; then
  echo "[$(date -Iseconds)] Step 3 skipped — $LANGUAGES_OUT already complete"
else
  echo "[$(date -Iseconds)] Step 3 — fetching $PENDING_COUNT repos for ${YM}"
  # awk/sort run on the host; their output is piped into the container via stdin.
  # Inter-request pacing is handled internally by the loader (adaptive cooldown).
  cat "$PENDING" \
    | docker_run language_loader github_language_loader -- \
    >> "$LANGUAGES_OUT"
  echo "[$(date -Iseconds)] Step 3 done → $LANGUAGES_OUT"
fi

# ── Step 4: per-month language ratings ─────────────────────────────────────
PR_OUT="data/stats/language-ratings-${YM}-pr-count.jsonl"

if [[ -f "$PR_OUT" ]]; then
  echo "[$(date -Iseconds)] Step 4 skipped — $PR_OUT already exists"
else
  echo "[$(date -Iseconds)] Step 4 — producing statistics for ${YM}"
  docker_run produce_statistics produce_statistics -- \
    --archive "$FILTERED_OUT" \
    --languages "$LANGUAGES_OUT" \
    --output-dir data/stats \
    --cap-single-actor-events
  echo "[$(date -Iseconds)] Step 4 done → data/stats/language-ratings-${YM}-*.jsonl"
fi

# ── Step 5: pack all-months files (refresh every run so the UI stays current)
echo "[$(date -Iseconds)] Step 5 — packing all-months rating files"
for TYPE in "${STAT_TYPES[@]}"; do
  docker_run "pack_${TYPE}" pack_statistics -- \
    --type "$TYPE" \
    --input-dir data/stats \
    --output-dir data/stats
done
echo "[$(date -Iseconds)] Step 5 done → data/stats/language-ratings-all-*.jsonl"

echo "[$(date -Iseconds)] collect_month.sh finished for ${YM}"
