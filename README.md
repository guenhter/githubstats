# githubstats

Collects monthly GitHub language statistics from the [GH Archive](https://www.gharchive.org/)
and the GitHub GraphQL API, producing per-language weighted activity ratings for a given month.

---

## Pipeline overview

Five tools run in sequence to produce language-rating files for a month:

```
github_archive_loader  →  data/archives/archive-YYYYMM.csv
        ↓
filter_archive         →  data/archives-filtered/archive-YYYYMM-filtered.csv
        ↓
github_language_loader →  data/languages/languages-YYYY-MM.jsonl
        ↓
produce_statistics     →  data/stats/language-ratings-YYYY-MM-<type>.jsonl
        ↓
pack_statistics        →  data/stats/language-ratings-all-<type>.jsonl
```


| Tool                     | Input                                                         | Output                                                                                                                                                                                                                                                                                                                                                             |
| ------------------------ | ------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `github_archive_loader`  | GH Archive hourly `.json.gz` files (downloaded automatically) | `data/archives/archive-YYYYMM.csv` (CSV) Sample: `actor,repo,event_type,action,language,count` `torvalds,torvalds/linux,PushEvent,,,42` `octocat,octocat/Hello-World,PullRequestEvent,opened,,3`                                                                                                                                                                   |
| `filter_archive`         | archive CSV                                                   | `data/archives-filtered/archive-YYYYMM-filtered.csv` (CSV) same format as above, with bots, CI actors, high-volume actors/push-repos, single-event repos, etc. removed                                                                                                                                                                                             |
| `github_language_loader` | stdin — one `owner/repo` slug per line                        | `data/languages/languages-YYYY-MM.jsonl` (JSONL) Sample: `{"repo":"torvalds/linux","total_size":1247804,"languages":[{"language":"C","size":1100000},{"language":"Shell","size":80000}],"fetched_at":"2026-08-02T17:54:00Z"}` `{"repo":"octocat/Hello-World","total_size":1024,"languages":[{"language":"Ruby","size":1024}],"fetched_at":"2026-08-02T17:54:00Z"}` |
| `produce_statistics`     | filtered archive CSV + languages JSONL                        | `data/stats/language-ratings-YYYY-MM-<type>.jsonl` (JSONL, one per statistic type) Sample: `{"language":"TypeScript","percentage":22.63,"rating":586871.81}` `{"language":"Python","percentage":15.94,"rating":413407.79}` `{"language":"JavaScript","percentage":11.12,"rating":288414.50}`                                                                       |
| `pack_statistics`        | per-month rating JSONL files                                  | `data/stats/language-ratings-all-<type>.jsonl` (JSONL, one per statistic type) Sample: `{"month":"2026-01","language":"TypeScript","percentage":22.63,"rating":586871.81}` `{"month":"2026-01","language":"Python","percentage":15.94,"rating":413407.79}` `{"month":"2026-02","language":"TypeScript","percentage":21.87,"rating":568204.13}`                     |


> **Required environment variable for** `github_language_loader`**:**
>
> ```bash
> export GITHUB_TOKEN=ghp_…   # GitHub PAT with public_repo read access
> ```

---



## Running the pipeline

```bash
YEAR=2026
MONTH=01          # zero-padded for file names

# Step 1 — download & aggregate GH Archive events for the month
cargo run --release --bin github_archive_loader -- \
  --year "$YEAR" \
  --month "$MONTH" \
  --parallelism 10 \
  --output "data/archives/archive-${YEAR}${MONTH}.csv"

# Step 2 — filter out bots, CI actors, noise repos
# Defaults: --actor-event-limit 1000 --repo-push-limit 100 --repo-min-events 10
cargo run --release --bin filter_archive -- \
  --input "data/archives/archive-${YEAR}${MONTH}.csv" \
  --output "data/archives-filtered/archive-${YEAR}${MONTH}-filtered.csv"

# Step 3 — resolve language breakdown for repos with any event type used by produce_statistics
#
# For archives up to and including September 2025 the GH Archive CSV already
# contains a language column (field 5).  Extract it directly with awk — no
# GitHub API calls required:
#
#   awk -F',' 'NR==1{next} $5==""{next} seen[$2]++{next} \
#     {printf "{\"repo\":\"%s\",\"total_size\":1,\"languages\":[{\"language\":\"%s\",\"size\":1}]}\n", $2, $5}' \
#     "data/archives/archive-${YEAR}${MONTH}.csv" > "data/languages/languages-${YEAR}-${MONTH}.jsonl"
#
# From October 2025 onwards GitHub stripped language from event payloads, so
# the GraphQL fallback below is required (see docs/LANGUAGE_DATA.md).
#
#   (extract unique repo slugs that had a counted event, skip the header)
export GITHUB_TOKEN=ghp_…
awk -F',' 'NR>1 && ($3 == "PullRequestEvent" || $3 == "IssuesEvent" || $3 == "PushEvent" || $3 == "WatchEvent") {print $2}' \
    "data/archives-filtered/archive-${YEAR}${MONTH}-filtered.csv" | sort -u \
  | cargo run --release --bin github_language_loader -- \
  > "data/languages/languages-${YEAR}-${MONTH}.jsonl"

# Step 4 — compute weighted per-language ratings (one output file per statistic type)
cargo run --release --bin produce_statistics -- \
  --archive "data/archives-filtered/archive-${YEAR}${MONTH}-filtered.csv" \
  --languages "data/languages/languages-${YEAR}-${MONTH}.jsonl" \
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



### Rating files

`produce_statistics` writes multiple JSONL files, all sorted descending by rating:


| File                                          | Signal                    | Formula                                                  |
| --------------------------------------------- | ------------------------- | -------------------------------------------------------- |
| `language-ratings-YYYY-MM-pr-count.jsonl`     | Pull-request volume       | `rating[L] += pr_count × (size_L / total_size)`          |
| `language-ratings-YYYY-MM-issue-count.jsonl`  | Issue volume              | `rating[L] += issue_count × (size_L / total_size)`       |
| `language-ratings-YYYY-MM-push-count.jsonl`   | Push volume               | `rating[L] += push_count × (size_L / total_size)`        |
| `language-ratings-YYYY-MM-active-repos.jsonl` | Active repository breadth | `rating[L] += 1 × (size_L / total_size)` per active repo |
| `language-ratings-YYYY-MM-star-count.jsonl`   | Stars (WatchEvents)       | `rating[L] += star_count × (size_L / total_size)`        |


Each record:

```json
{"language":"TypeScript","rating":322361.9}
```

`pack_statistics` merges all monthly files for a given type into one combined file:


| File                                      | Contents                                         |
| ----------------------------------------- | ------------------------------------------------ |
| `language-ratings-all-pr-count.jsonl`     | All months, pr-count, sorted chronologically     |
| `language-ratings-all-issue-count.jsonl`  | All months, issue-count, sorted chronologically  |
| `language-ratings-all-push-count.jsonl`   | All months, push-count, sorted chronologically   |
| `language-ratings-all-active-repos.jsonl` | All months, active-repos, sorted chronologically |
| `language-ratings-all-star-count.jsonl`   | All months, star-count, sorted chronologically   |


Each record has a `month` field prepended:

```json
{"month":"2026-01","language":"TypeScript","rating":322361.9}
```

---



## Rating formula

For each repository with language breakdown `{L: size_L}` and total codebase size `total_size`:

```
rating[L] += event_count × (size_L / total_size)
```

Example: a repo with 2 PRs that is 70% TypeScript / 30% Python contributes
**1.4** to TypeScript and **0.6** to Python.

The `active-repos` variant contributes exactly **1 per repository** (regardless
of event volume) to each of that repository's languages by byte share. This
measures breadth of language adoption — how many distinct active codebases use
each language — rather than the volume of activity those codebases generate.