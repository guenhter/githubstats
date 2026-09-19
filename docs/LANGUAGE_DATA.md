# Language data sources and attribution

## Why GraphQL for language data?

The GH Archive publishes every public GitHub event as hourly gzip-compressed
NDJSON files (`YYYY-MM-DD-H.json.gz`). Those files tell you *what happened* and
*on which repository*, but not a reliable multi-language breakdown of the repo.

Until September 2025, some event payloads included a single primary-language
string (for example `pull_request.base.repo.language`). **From October 2025
onwards GitHub stripped those payload fields.** A 2026 `PullRequestEvent`
contains only the PR URL, number, and the head/base ref and SHA — no language,
no line counts, no merge flag.

This project always resolves languages the same way: `repo_language_loader`
queries the GitHub GraphQL API for each repo's language byte breakdown and
writes `data/repo-languages/repo-languages-YYYY-MM.jsonl`.

### Official reference

**[Upcoming changes to GitHub Events API payloads](https://github.blog/changelog/2025-08-08-upcoming-changes-to-github-events-api-payloads/)**
— GitHub Changelog, August 8, 2025. Rollout date: **October 7, 2025**.

Community impact documented in:
[Data size / number of events have dropped 100x since 2025-10-09](https://github.com/igrigorik/gharchive.org/issues/312)

---

## Why not just use Google BigQuery for GH Archive data?

The GH Archive data is also available via Google BigQuery (`githubarchive` public dataset),
which allows SQL queries over the full event history without downloading any files.
There are two reasons this project downloads the raw `.json.gz` files directly instead:

- **Cost.** BigQuery charges per byte scanned. A single month of GH Archive data is
  several hundred gigabytes; querying it repeatedly across many months adds up quickly.
  Downloading the hourly files is free.

- **Payload stripping.** BigQuery mirrors whatever GH Archive publishes. From October 2025
  onwards the payloads are already stripped (no language field) before they reach BigQuery,
  so the same GraphQL language-lookup step is still required. BigQuery offers no
  advantage for language attribution.

---

## Experiment: proportional weighting vs primary-language-only

An earlier experimental `produce_statistics --primary-only` mode attributed
all of a repo's score to its single dominant (largest-by-bytes) language,
ignoring secondary languages entirely. That flag is no longer in the binary;
the comparison below is kept as a historical result.

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
