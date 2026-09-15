# Filter rationale

This document explains the volume / automation filters in `filter_archive`
that matter most for language ratings after the Oct 2025 GH Archive payload
change (when language attribution moved to GraphQL). The binary also runs
many other heuristics (empty repos, CI name lists, low-activity tails, etc.);
see `src/bin/filter_archive.rs` for the full chain.

A continuity experiment (require repos in the previous two raw months) was
tried and dropped: with bots + actor/push caps in place, ratings with and
without continuity were nearly the same, while continuity discarded most
repos and complicated GraphQL coverage.

---

## Bot filter (`filter_bots`)

**Rule:** drop any row whose `actor` contains `"bot"` (case-insensitive).

**Why:** in 2021, Dependabot (and similar `*[bot]` accounts) opened millions
of dependency-bump PRs, overwhelmingly on npm / JavaScript repos, producing
huge JS spikes on PR and active-repo metrics that were not a real language
shift.

Named-bot filtering removes that class of automation. It does **not** catch
human usernames that run scrapers or dashboards (no `"bot"` substring).

---

## High-volume actor filter (`filter_high_volume_actors`)

**Rule:** drop **all** rows from any actor whose total event `count` in the
month exceeds `--actor-event-limit` (default **1000**).

**Why:** mid–late 2026 HTML **push-count** spikes were driven by single
human-looking accounts flooding HTML-only dashboards / scrapers / digests
with thousands of pushes. Capping per-actor monthly volume stops one
scripted login from swinging language shares.

```bash
filter_archive ... --actor-event-limit 1000   # default
```

---

## High-volume push-repo filter (`filter_high_volume_push_repos`)

**Rule:** drop **PushEvent** rows from any repo whose total PushEvent `count`
exceeds `--repo-push-limit` (default **100**). Other event types (PR, issue,
star, …) for that repo are kept — same shape as
`filter_high_volume_issue_repos`.

**Why:** after the per-actor ceiling, a swarm of near-cap single-actor
HTML/scraper repos (100–1000 pushes/month) still inflated push-count.
A per-repo push ceiling removes that class without language data at filter
time.

```bash
filter_archive ... --repo-push-limit 100   # default
```

---

## Related: single-actor scoring cap

At produce time, `produce_statistics --cap-single-actor-events` caps
pr-count / push-count at 1 for repos with exactly one distinct actor.
That is a scoring rule, not an archive filter; it keeps the repos but
removes remaining push-mill volume from the ratings.

---

## Recommended pipeline (post–Oct 2025)

```bash
filter_archive \
  --input  data/archives/archive-YYYYMM.csv \
  --output data/archives-filtered/archive-YYYYMM-filtered.csv
  # defaults: --actor-event-limit 1000 --repo-push-limit 100 …

produce_statistics \
  --archive data/archives-filtered/archive-YYYYMM-filtered.csv \
  --languages data/languages/languages-YYYY-MM.jsonl \
  --output-dir data/stats \
  --cap-single-actor-events
```
