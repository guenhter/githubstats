# githubstats

Collects monthly GitHub language statistics from the [GH Archive](https://www.gharchive.org/)
and the GitHub GraphQL API, producing per-language weighted activity ratings for a given month.

---

## Pipeline overview

Four tools run in sequence to produce language-rating files for a month:

```
github_archive_loader  →  archive-YYYYMM.csv
        ↓
filter_csv             →  archive-YYYYMM-filtered.csv
        ↓  (extract repo slugs)
github_language_loader →  languages-YYYY-MM.jsonl
        ↓
produce_statistics     →  language-ratings-YYYY-MM-<type>.jsonl  (four files)
```

| Tool | Input | Output |
|---|---|---|
| `github_archive_loader` | GH Archive hourly `.json.gz` files (downloaded automatically) | CSV — `actor,repo,event_type,action,language,count` |
| `filter_csv` | archive CSV | filtered CSV — bots, CI actors, single-event repos and deleted repos removed |
| `github_language_loader` | stdin — one `owner/repo` slug per line | stdout JSONL — `{"repo":"…","total_size":N,"languages":[…]}` |
| `produce_statistics` | archive CSV + languages JSONL | four JSONL files in `--output-dir`, one per statistic type |

> **Required environment variable for `github_language_loader`:**
> ```bash
> export GITHUB_TOKEN=ghp_…   # GitHub PAT with public_repo read access
> ```

---

## Running the pipeline

```bash
YEAR=2026
MONTH=01          # zero-padded for file names
MONTH_DASHED=2026-01

# Step 1 — download & aggregate GH Archive events for the month
cargo run --release --bin github_archive_loader -- \
  --year "$YEAR" \
  --month "$((10#$MONTH))" \
  --parallelism 10 \
  --output "data/archive-${YEAR}${MONTH}.csv"

# Step 2 — filter out bots, CI actors, noise repos
cargo run --release --bin filter_csv -- \
  --input "data/archive-${YEAR}${MONTH}.csv"
# produces: data/archive-${YEAR}${MONTH}-filtered.csv

# Step 3 — resolve language breakdown for repos with PR activity
#   (extract unique repo slugs that had PullRequestEvents, skip the header)
export GITHUB_TOKEN=ghp_…
awk -F',' 'NR>1 && $3=="PullRequestEvent" {print $2}' \
    "data/archive-${YEAR}${MONTH}-filtered.csv" | sort -u \
  | cargo run --release --bin github_language_loader -- \
      --workers 10 \
  > "data/languages-${MONTH_DASHED}.jsonl"

# Step 4 — compute weighted per-language ratings (four output files)
cargo run --release --bin produce_statistics -- \
  --archive "data/archive-${YEAR}${MONTH}-filtered.csv" \
  --languages "data/languages-${MONTH_DASHED}.jsonl" \
  --output-dir data/
```

### Output files

`produce_statistics` writes four JSONL files, all sorted descending by rating:

| File | Signal | Formula |
|---|---|---|
| `language-ratings-YYYY-MM-pr-count.jsonl` | Pull-request volume | `rating[L] += pr_count × (size_L / total_size)` |
| `language-ratings-YYYY-MM-issue-count.jsonl` | Issue volume | `rating[L] += issue_count × (size_L / total_size)` |
| `language-ratings-YYYY-MM-push-count.jsonl` | Push volume | `rating[L] += push_count × (size_L / total_size)` |
| `language-ratings-YYYY-MM-developer-activity.jsonl` | Distinct PR contributors | `rating[L] += distinct_pr_actors × (size_L / total_size)` |

Each record:
```json
{"language":"TypeScript","rating":322361.9}
```

---

## Rating formula

For each repository with language breakdown `{L: size_L}` and total codebase size `total_size`:

```
rating[L] += event_count × (size_L / total_size)
```

Example: a repo with 10 PRs that is 70% TypeScript / 30% Python contributes
**7.0** to TypeScript and **3.0** to Python.

The `developer-activity` variant uses **distinct PR contributors** instead of
raw PR count, making it neutral to per-developer commit-frequency habits.

---

## Event types

The archive CSV contains one row per unique `(actor, repo, event_type, action)` tuple per month.
`produce_statistics` uses three event types:

| Event type | Used for |
|---|---|
| `PullRequestEvent` | `pr-count`, `developer-activity` |
| `IssuesEvent` | `issue-count` |
| `PushEvent` | `push-count` |

Full list of event types that appear in GH Archive data:

| Event type | Actions | Description |
|---|---|---|
| `CommitCommentEvent` | `created` | A comment was posted on a commit. |
| `CreateEvent` | *(none)* | A branch, tag, or repository was created. |
| `DeleteEvent` | *(none)* | A branch or tag was deleted. |
| `DiscussionEvent` | `created` | A discussion was created in a repository. |
| `ForkEvent` | `forked` | A user forked a repository. |
| `GollumEvent` | *(none)* | A wiki page was created or updated. |
| `IssueCommentEvent` | `created` | A comment was posted on an issue or pull request. |
| `IssuesEvent` | `opened`, `closed`, `reopened`, `assigned`, `unassigned`, `labeled`, `unlabeled` | Activity on an issue. |
| `MemberEvent` | `added` | A collaborator was added to a repository. |
| `PublicEvent` | *(none)* | A private repository was made public. |
| `PullRequestEvent` | `opened`, `closed`, `reopened`, `assigned`, `unassigned`, `labeled`, `unlabeled` | Activity on a pull request. |
| `PullRequestReviewEvent` | `created`, `updated`, `dismissed` | A pull request review was submitted, updated, or dismissed. |
| `PullRequestReviewCommentEvent` | `created` | A comment was posted on a pull request diff. |
| `PushEvent` | *(none)* | One or more commits were pushed to a branch or tag. |
| `ReleaseEvent` | `published` | A release was published. |
| `WatchEvent` | `started` | A user starred a repository (the API calls this "watching"). |

> **Note:** Events with no action have an empty `action` column in the CSV.

---

## Why not just use the GH Archive for language data?

The GH Archive publishes every public GitHub event as hourly gzip-compressed
NDJSON files (`YYYY-MM-DD-H.json.gz`). Until September 2025 those files
contained rich payloads — including `pull_request.base.repo.language` —
which made full language attribution possible with zero external API calls.

**From October 2025 onwards GitHub stripped those payload fields.** A 2026
`PullRequestEvent` contains only the PR URL, number, and the head/base ref
and SHA. No language. No line counts. No merge flag.

This means for 2026 data the archive alone can tell you *what happened* and
*on which repository*, but never *in which language* — hence the GraphQL
language-lookup step.

### Official reference

**[Upcoming changes to GitHub Events API payloads](https://github.blog/changelog/2025-08-08-upcoming-changes-to-github-events-api-payloads/)**
— GitHub Changelog, August 8, 2025. Rollout date: **October 7, 2025**.

Community impact documented in:
[Data size / number of events have dropped 100x since 2025-10-09](https://github.com/igrigorik/gharchive.org/issues/312)

---

## Experiment: proportional weighting vs primary-language-only

`produce_statistics --primary-only` is an experimental mode that attributes
all of a repo's score to its single dominant (largest-by-bytes) language,
ignoring secondary languages entirely.  Output filenames gain a `-primary`
suffix (e.g. `language-ratings-2024-01-pr-count-primary.jsonl`).

### What changes

| Language type | Proportional | Primary-only |
|---|---|---|
| Markup/tooling (CSS, HTML, Shell, SCSS, Makefile, Dockerfile) | Receive fractional credit from mixed repos | Drop sharply — rarely the primary language |
| Pure-language repos (Go, Rust, Java, C#, PHP) | Already near-primary | Gain 8–16% |
| Dominant-ecosystem languages (TypeScript, Python) | Top two in both modes | Gain 5–7% |
| Multi-language glue (JavaScript) | Slight loss (~2–4%) | Slight loss |

### Top-30 comparison — 2024-01, pr-count

| Rank | Language | Proportional | Primary-only | Rank Δ | Rating Δ |
|---:|---|---:|---:|---:|---:|
| 1 | TypeScript | 206,502 | 216,060 | = | +4.6% |
| 2 | Python | 170,482 | 180,602 | = | +5.9% |
| 3 | JavaScript | 114,230 | 109,874 | = | −3.8% |
| 4 | Go | 95,636 | 105,473 | = | +10.3% |
| 5 | Java | 87,186 | 95,268 | = | +9.3% |
| 6 | C++ | 68,941 | 75,499 | = | +9.5% |
| 7 | Rust | 60,939 | 66,430 | = | +9.0% |
| 8 | HTML | 43,414 | 36,698 | −1 | −15.5% |
| 9 | C | 37,608 | 35,261 | −1 | −6.2% |
| 10 | C# | 34,118 | 37,342 | +2 | +9.5% |
| 11 | Shell | 26,654 | 20,116 | −1 | −24.5% |
| 12 | PHP | 23,230 | 28,068 | +1 | +20.8% |
| 13 | Kotlin | 20,039 | 19,470 | = | −2.8% |
| 14 | CSS | 18,835 | 9,389 | −4 | **−50.2%** |
| 15 | Ruby | 16,826 | 18,685 | +1 | +11.0% |
| 16 | Jupyter Notebook | 16,542 | 16,169 | +1 | −2.3% |
| 17 | MDX | 11,807 | 10,105 | = | −14.4% |
| 18 | Swift | 10,403 | 10,522 | +2 | +1.1% |
| 19 | Vue | 9,743 | 7,862 | −2 | −19.3% |
| 20 | Dart | 8,138 | 8,906 | +1 | +9.4% |
| 21 | Nix | 7,669 | 7,942 | +1 | +3.6% |
| 22 | SCSS | 7,643 | — | dropped | — |
| 23 | DM | 6,774 | 7,476 | +1 | +10.4% |
| 24 | Lua | 6,308 | 6,307 | = | −0.0% |
| 25 | Scala | 6,062 | 6,473 | +2 | +6.8% |
| 26 | Markdown | 4,412 | 4,468 | = | +1.3% |
| 27 | HCL | 4,356 | 4,323 | = | −0.7% |
| 28 | Solidity | 4,053 | 3,036 | −2 | −25.1% |
| 29 | Makefile | 4,022 | — | dropped | — |
| 30 | Svelte | 3,538 | — | dropped | — |
| — | LLVM | — | 6,107 | enters top-30 | — |
| — | Julia | — | 3,634 | enters top-30 | — |
| — | Haskell | — | 3,478 | enters top-30 | — |

### Top-30 comparison — 2026-01, pr-count

| Rank | Language | Proportional | Primary-only | Rank Δ | Rating Δ |
|---:|---|---:|---:|---:|---:|
| 1 | TypeScript | 322,362 | 339,471 | = | +5.3% |
| 2 | Python | 202,076 | 216,536 | = | +7.2% |
| 3 | JavaScript | 134,815 | 134,785 | = | −0.0% |
| 4 | Java | 81,936 | 88,022 | = | +7.4% |
| 5 | HTML | 70,335 | 59,251 | −2 | −15.8% |
| 6 | Go | 57,376 | 63,217 | +1 | +10.2% |
| 7 | Rust | 55,010 | 61,076 | +1 | +11.0% |
| 8 | C++ | 48,278 | 52,468 | = | +8.7% |
| 9 | C# | 37,051 | 41,320 | = | +11.5% |
| 10 | PHP | 27,942 | 32,319 | = | +15.7% |
| 11 | CSS | 27,602 | 5,935 | −10 | **−78.5%** |
| 12 | Shell | 27,037 | 20,152 | −1 | −25.5% |
| 13 | C | 22,207 | 20,818 | +1 | −6.3% |
| 14 | Kotlin | 20,810 | 20,933 | +3 | +0.6% |
| 15 | Swift | 12,588 | 12,209 | −1 | −3.0% |
| 16 | Dart | 11,557 | 13,065 | +2 | +13.0% |
| 17 | Vue | 11,370 | 9,731 | = | −14.4% |
| 18 | Ruby | 10,819 | 12,804 | +3 | +18.4% |
| 19 | Jupyter Notebook | 8,686 | 8,057 | = | −7.2% |
| 20 | MDX | 8,226 | 6,187 | = | −24.8% |
| 21 | Nix | 7,708 | 8,374 | +3 | +8.6% |
| 22 | HCL | 6,012 | 5,638 | = | −6.2% |
| 23 | SCSS | 4,595 | — | dropped | — |
| 24 | Lua | 4,216 | 4,184 | +1 | −0.8% |
| 25 | Blade | 3,691 | 3,667 | = | −0.7% |
| 26 | DM | 3,424 | 3,791 | +2 | +10.7% |
| 27 | Svelte | 3,154 | 2,175 | −3 | −31.0% |
| 28 | PLpgSQL | 2,875 | — | dropped | — |
| 29 | Astro | 2,862 | 3,205 | +3 | +12.0% |
| 30 | Scala | 2,845 | 2,993 | +3 | +5.2% |
| — | LLVM | — | 2,899 | enters top-30 | — |
| — | GDScript | — | 2,729 | enters top-30 | — |

### Conclusion

The top-order ranking is **largely stable** between the two methods.
The dominant languages (TypeScript, Python, JavaScript, Go, Java) hold
their positions in both months under both formulas.

The meaningful differences are:

1. **Markup/tooling languages drop sharply in primary-only mode.**
   CSS loses 50–78%, Shell 24–26%, SCSS and Makefile fall out of the top 30
   entirely. These languages are almost always secondary in mixed repos, so
   their proportional score is mainly borrowed from other codebases.

2. **Pure-ecosystem languages gain modestly.**
   Go, Rust, Java, C#, PHP each gain 8–16% because they tend to be the sole
   or dominant language in their repos — they were already getting most of
   the proportional credit.

3. **Systems languages enter the top 30 only in primary-only mode.**
   LLVM, Julia, Haskell, and GDScript appear in the primary-only top 30.
   Their repos are dedicated to a single language, so they benefit most from
   eliminating fractional dilution.

4. **The proportional formula is more informative** for understanding
   real-world language mix. A TypeScript repo that embeds 30% CSS
   genuinely represents CSS work; discarding that credit understates CSS
   activity. Primary-only is better treated as a "dominant language" index
   rather than a general activity index.
