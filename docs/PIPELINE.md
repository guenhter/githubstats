# Pipeline

Five tools run in sequence to produce language-rating files for a month:

```
event_loader           →  data/events/events-YYYY-MM.csv
        ↓
filter_events          →  data/events-filtered/events-YYYY-MM.csv
        ↓
repo_language_loader   →  data/repo-languages/repo-languages-YYYY-MM.jsonl
        ↓
produce_statistics     →  data/stats/language-ratings-YYYY-MM-<type>.jsonl
        ↓
pack_statistics        →  data/stats/language-ratings-all-<type>.jsonl
```

| Tool | Input | Output |
| --- | --- | --- |
| `event_loader` | GH Archive hourly `.json.gz` files (downloaded automatically) | `data/events/events-YYYY-MM.csv` — `actor,repo,event_type,action,language,count` |
| `filter_events` | events CSV | `data/events-filtered/events-YYYY-MM.csv` — same format; bots, CI actors, high-volume actors/push-repos, single-event repos, etc. removed |
| `repo_language_loader` | stdin — one `owner/repo` slug per line | `data/repo-languages/repo-languages-YYYY-MM.jsonl` — repo → language byte breakdown |
| `produce_statistics` | filtered events CSV + repo-languages JSONL | `data/stats/language-ratings-YYYY-MM-<type>.jsonl` — one file per statistic type |
| `pack_statistics` | per-month rating JSONL files | `data/stats/language-ratings-all-<type>.jsonl` — all months, one file per type |

> **Required environment variable for** `repo_language_loader`**:**
>
> ```bash
> export GITHUB_TOKEN=ghp_…   # GitHub PAT with public_repo read access
> ```

For a full month end-to-end, prefer [`collect_month.sh`](../collect_month.sh) (cron-oriented). The steps below are the same pipeline invoked manually.

## Running the pipeline

```bash
YEAR=2026
MONTH=01          # zero-padded for file names

# Step 1 — download & aggregate GH Archive events for the month
cargo run --release --bin event_loader -- \
  --year "$YEAR" \
  --month "$MONTH" \
  --parallelism 10 \
  --output "data/events/events-${YEAR}-${MONTH}.csv"

# Step 2 — filter out bots, CI actors, noise repos
# Defaults: --actor-event-limit 1000 --repo-push-limit 100 --repo-min-events 10
cargo run --release --bin filter_events -- \
  --input "data/events/events-${YEAR}-${MONTH}.csv" \
  --output "data/events-filtered/events-${YEAR}-${MONTH}.csv"

# Step 3 — resolve language breakdown via GitHub GraphQL
# Unique repo slugs from the filtered file (filter_no_code_activity_repos already
# drops any repo without Push/PR activity; see LANGUAGE_DATA.md).
export GITHUB_TOKEN=ghp_…
awk -F',' 'NR>1 {print $2}' \
    "data/events-filtered/events-${YEAR}-${MONTH}.csv" | sort -u \
  | cargo run --release --bin repo_language_loader -- \
  > "data/repo-languages/repo-languages-${YEAR}-${MONTH}.jsonl"

# Step 4 — compute weighted per-language ratings (one output file per statistic type)
cargo run --release --bin produce_statistics -- \
  --events "data/events-filtered/events-${YEAR}-${MONTH}.csv" \
  --repo-languages "data/repo-languages/repo-languages-${YEAR}-${MONTH}.jsonl" \
  --output-dir data/stats \
  --cap-single-actor-events

# Step 5 — pack all monthly ratings into combined files (run once after all months are produced)
for TYPE in pr-count issue-count push-count active-repos star-count; do
  cargo run --release --bin pack_statistics -- \
    --type "$TYPE" \
    --input-dir data/stats \
    --output-dir data/stats
done
```

## Rating files

`produce_statistics` writes multiple JSONL files, all sorted descending by rating:

| File | Contents |
| --- | --- |
| `language-ratings-YYYY-MM-pr-count.jsonl` | Per-month pull-request ratings |
| `language-ratings-YYYY-MM-issue-count.jsonl` | Per-month issue ratings |
| `language-ratings-YYYY-MM-push-count.jsonl` | Per-month push ratings |
| `language-ratings-YYYY-MM-active-repos.jsonl` | Per-month active-repo ratings |
| `language-ratings-YYYY-MM-star-count.jsonl` | Per-month star ratings |

Each record:

```json
{"language":"TypeScript","percentage":22.63,"rating":586871.81}
```

`pack_statistics` merges all monthly files for a given type into one combined file:

| File | Contents |
| --- | --- |
| `language-ratings-all-pr-count.jsonl` | All months, pr-count, sorted chronologically |
| `language-ratings-all-issue-count.jsonl` | All months, issue-count, sorted chronologically |
| `language-ratings-all-push-count.jsonl` | All months, push-count, sorted chronologically |
| `language-ratings-all-active-repos.jsonl` | All months, active-repos, sorted chronologically |
| `language-ratings-all-star-count.jsonl` | All months, star-count, sorted chronologically |

Each record has a `month` field prepended:

```json
{"month":"2026-01","language":"TypeScript","percentage":22.63,"rating":586871.81}
```
