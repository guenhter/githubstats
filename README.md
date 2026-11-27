# GitHub Language Trends

![Live site](https://img.shields.io/badge/site-guenhter.github.io-blue?logo=github)
![License: MIT](https://img.shields.io/badge/license-MIT-green)

**[View live site →](https://guenhter.github.io/githubstats/)**

**githubstats** — monthly programming-language popularity from real GitHub activity, spanning January 2015 through the present.
Events come from [GH Archive](https://www.gharchive.org/); bots and automation are filtered out; languages are resolved from the GitHub GraphQL API by byte share, not a single primary-language field.
Explore the interactive chart and ranked table across five activity metrics: pull requests, pushes, issues, active repos, and stars.

![GitHub Language Trends — pull-request share over time](docs/screenshot.png)

## Try it

Open the **[live site](https://guenhter.github.io/githubstats/)**, or serve the repo locally (not `file://` — the browser needs HTTP for `fetch` and the ECharts CDN):

```bash
python3 -m http.server 8080 --directory .
```

Then open http://localhost:8080/index.html.

## How it works

Event counts are attributed by each **repository’s** language mix (byte share from the GitHub GraphQL API), not by the languages changed in the event itself. A PR in a mostly-TypeScript repo credits TypeScript even if the diff touched Python.

```
rating[L] += event_count × (size_L / total_size)
```

Example with two repos and three PRs:

| Repo | Events (PR, Push, ...) | Python | TypeScript |
| --- | ---: | ---: | ---: |
| RepoA (30% Py / 70% TS) | 2 | 2 × 0.3 | 2 × 0.7 |
| RepoB (100% TS) | 1 | — | 1 × 1.0 |
| **Total** |  | **0.6 (20%)** | **2.4 (80%)** |

Five signals use that weighting (or a unit weight for breadth):

| Metric | Signal |
| --- | --- |
| `pr-count` | Pull-request volume |
| `issue-count` | Issue volume |
| `push-count` | Push volume |
| `active-repos` | Distinct active repos (breadth, not volume) |
| `star-count` | Stars (WatchEvents) |

More detail: [`docs/LANGUAGE_DATA.md`](docs/LANGUAGE_DATA.md) (language attribution), [`docs/PIPELINE.md`](docs/PIPELINE.md) (tools, runbook, rating file formats), [`docs/HEADLINE.md`](docs/HEADLINE.md) (monthly headline rules).
