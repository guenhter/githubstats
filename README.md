# githubstats

Collects monthly GitHub language statistics from the [GH Archive](https://www.gharchive.org/)
and the GitHub GraphQL API, producing per-language weighted activity ratings for a given month.

---

## Pipeline overview

Three tools run in sequence to produce a language-rating file for a month:

```
github_archive_loader  →  archive-YYYYMM.csv
        ↓  (extract repo slugs)
github_language_loader →  projects-languages-YYYY-MM.jsonl
        ↓
produce_statistics     →  language-ratings-YYYY-MM.jsonl
```

| Tool | Input | Output |
|---|---|---|
| `github_archive_loader` | GH Archive hourly `.json.gz` files (downloaded automatically) | CSV — `repo,event_type,action,language,count` |
| `github_language_loader` | stdin — one `owner/repo` slug per line | stdout JSONL — `{"repo":"…","languages":[…]}` |
| `produce_statistics` | archive CSV + languages JSONL | stdout JSONL — `{"language":"Rust","rating":14781.33}` |

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

# Step 2 — resolve the language breakdown for every repo found in the CSV
#   (extract unique repo slugs with awk, skip the header, skip blank language column)
export GITHUB_TOKEN=ghp_…
awk -F',' 'NR>1 {print $1}' "data/archive-${YEAR}${MONTH}.csv" | sort -u \
  | cargo run --release --bin github_language_loader -- \
      --workers 10 \
  > "data/projects-languages-${MONTH_DASHED}.jsonl"

# Step 3 — compute weighted per-language ratings
cargo run --release --bin produce_statistics -- \
  --archive "data/archive-${YEAR}${MONTH}.csv" \
  --languages "data/projects-languages-${MONTH_DASHED}.jsonl" \
  > "data/language-ratings-${MONTH_DASHED}.jsonl"
```

### Sample output (`language-ratings-2026-01.jsonl`)

```json
{"language":"TypeScript","rating":75621.65}
{"language":"Python","rating":47307.61}
{"language":"JavaScript","rating":26960.70}
```

The rating for each language is the sum of `PR count × language share (%)` across all matched repositories.

---

## Rating formula

For each repository *P* with *N* merged pull-request events:

```
For each language L used by P at percentage pct:
    language_rating[L] += N × (pct / 100)
```

Example: a repo with 10 PRs that is 70 % Python contributes **7.0** to the Python rating.

---

## Event types

The archive CSV contains one row per unique `(repo, event_type, action)` triple per month.
The event types that appear in GH Archive data are a subset of the GitHub Events API:

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
| `PullRequestEvent` | `opened`, `closed`, `reopened`, `assigned`, `unassigned`, `labeled`, `unlabeled` | Activity on a pull request. A `closed` PR with `pull_request.merged = true` in the payload means it was merged. |
| `PullRequestReviewEvent` | `created`, `updated`, `dismissed` | A pull request review was submitted, updated, or dismissed. |
| `PullRequestReviewCommentEvent` | `created` | A comment was posted on a pull request diff. |
| `PushEvent` | *(none)* | One or more commits were pushed to a branch or tag. |
| `ReleaseEvent` | `published` | A release was published. |
| `WatchEvent` | `started` | A user starred a repository (the API calls this "watching"). |

> **Note:** Events with no action have an empty `action` column in the CSV.
> The `PushEvent` is by far the most frequent; `WatchEvent` (`started`) is a reliable proxy for stars.

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
