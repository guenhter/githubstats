# AGENTS.md

## Project-Specific Instructions

### What This Project Is

**githubstats** is a data pipeline and interactive visualization for GitHub programming language popularity, derived from real developer activity on [GH Archive](https://www.gharchive.org/).

The pipeline downloads every public GitHub event for a given month, filters out bots and automated noise, resolves language compositions per repository, and computes multiple weighted language activity ratings. The results are consumed by a single-page web UI (`index.html`) that renders an interactive trend chart (via Apache ECharts) and a ranked table with sparklines.

The project covers January 2015 through the present (~11 years of monthly data).
Language attribution always comes from the GitHub GraphQL API via `repo_language_loader`
(see `docs/LANGUAGE_DATA.md`). Event payloads used to embed a primary-language field
until October 2025; that field is not used by the pipeline.

### Technology Stack

- **Backend:** Rust (Edition 2024), async with `tokio`, HTTP with `reqwest`, JSON with `serde_json`, CLI with `clap`
- **Frontend:** Pure HTML + CSS + vanilla JavaScript, no build step; Apache ECharts 5 loaded from CDN
- **CI:** GitHub Actions — `cargo fmt --check` + `cargo clippy -D warnings` on every push/PR

### Project Structure

```
githubstats/
├── src/bin/                    # Five standalone CLI binaries (the entire backend)
│   ├── event_loader.rs            # Step 1: GH Archive → aggregated events CSV
│   ├── filter_events.rs           # Step 2: filtered events CSV (bot/noise removal)
│   ├── repo_language_loader.rs    # Step 3: repo slugs → GitHub GraphQL → language JSONL
│   ├── produce_statistics.rs      # Step 4: events CSV + languages → 5 per-month rating files
│   └── pack_statistics.rs         # Step 5: per-month files → 5 combined all-months files
├── data/                       # All intermediate and final data (large; partially gitignored)
│   ├── events/events-YYYY-MM.csv                            # Gitignored; raw monthly event aggregates
│   ├── events-filtered/events-YYYY-MM.csv                   # Gitignored; post-filter CSVs
│   ├── repo-languages/repo-languages-YYYY-MM.jsonl          # Gitignored; repo → language breakdown
│   └── stats/language-ratings-{YYYY-MM,all}-<type>.jsonl    # Ratings consumed by the frontend
├── docs/                       # Reference docs and sample event payloads
│   ├── GITHUB_EVENT_TYPES.md
│   └── events/{2024,2026}/     # Sample pre/post API-change payloads
├── index.html                  # Frontend SPA — no build step, open directly in browser
├── Cargo.toml                  # Package name is `fetch_month` (historical); defines all 5 binaries
```

**Where to put things:**
- New pipeline steps → new file under `src/bin/`
- Shared utilities → if significant, extract to `src/lib.rs` (no lib crate exists yet; keep code self-contained per binary unless sharing is clearly warranted)
- New data files → `data/`, following the existing naming conventions (see below)
- Reference documentation → `docs/`
- Frontend changes → `index.html` only (no build tooling)

### Data File Naming Conventions

| File | Pattern |
|---|---|
| Raw events (gitignored) | `data/events/events-YYYY-MM.csv` |
| Filtered events (gitignored) | `data/events-filtered/events-YYYY-MM.csv` |
| Repo language lookup (gitignored) | `data/repo-languages/repo-languages-YYYY-MM.jsonl` |
| Per-month rating | `data/stats/language-ratings-YYYY-MM-<type>.jsonl` |
| Combined all-months rating | `data/stats/language-ratings-all-<type>.jsonl` |

**Valid `<type>` values:** `pr-count`, `issue-count`, `push-count`, `active-repos`, `star-count`

### Build and Run Commands

```bash
# Build all binaries
cargo build --release

# Lint (matches CI exactly)
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings

# Run tests
cargo test
```

Individual pipeline step examples are documented in `README.md`.

### Key Architecture Patterns

**Self-contained binaries:** Each binary under `src/bin/` is fully self-contained. There is intentionally no shared library crate. Prefer this pattern — duplicate small utilities rather than introducing premature abstractions.

**Staged concurrent pipeline (`event_loader`):** Uses bounded `async-channel` MPMC channels between stages. Network I/O is async; CPU-intensive work (gzip decompression, JSON parsing) runs on `tokio::task::spawn_blocking`. This pattern must be preserved to avoid blocking the async executor.

**stderr for diagnostics, stdout for data (`repo_language_loader`):** All progress output goes to `stderr`; all data output goes to `stdout` as clean JSONL. This enables shell piping and must be maintained for any binary that reads/writes data streams.

**Two-phase filters (`filter_events`):** Every `filter_*` is `&[&Row]` (+ optional thresholds) → `Vec<&Row>` (borrows from the owned CSV; no `Row` clones). Independent filters always see the **original** row set; `run` intersects their kept-ref sets by address so aggregate limits are not falsified by earlier cuts and each filter’s log is relative to the full input. `filter_no_code_activity_repos` then runs as `rows = filter_no_code_activity_repos(&rows)` on post-intersection survivors so leftovers with no remaining Push/PR are dropped. Every filter must log a `[filter_name] N removed (X.X%), M remaining` line to stderr (independent filters vs the original set; no-code-activity-repos vs its input rows).

**Retry with exponential back-off:** Both HTTP clients implement manual retry (no middleware). New HTTP calls should follow the same pattern.

### Coding Conventions

- `anyhow::Result` for all fallible functions; `.context("...")` at every I/O site; no `unwrap()` in production paths
- Every binary opens with a `//!` module doc comment documenting: purpose, pipeline stage, input/output format, usage example, and design decisions
- Inline comments explain *why*, not *what* — especially for non-obvious decisions (API quirks, exploit mitigations, concurrency choices)
- Progress lines use bracketed prefixes: `[stage_name]`, `[retry N/M]`, `[rate-limit]`, etc.
- CLI struct is always named `Args` and derived with `clap::Parser`
- Constants at the top of each file with explanatory comments

### Environment Variables

| Variable | Required by | Purpose |
|---|---|---|
| `GITHUB_TOKEN` | `repo_language_loader` | GitHub PAT for GraphQL API (public repo read access sufficient) |

### Frontend Testing

After **any** change to `index.html`, verify with the integrated browser tool.
Do not use `file://` — ECharts and `fetch` require HTTP.

**Setup**

1. Start a local HTTP server (reuse one if already running):
   ```bash
   python3 -m http.server 8080 --directory . &
   ```
   If port 8080 is taken, try 8081, 8082, etc.
2. Navigate to `http://localhost:8080/index.html` (adjust port if needed).
3. Run the checklist below on **desktop** and **mobile** viewports
   (e.g. ~1280×900 and ~390×844 via device metrics override).
4. Confirm no JavaScript errors in the browser console.
5. Stop the server if you started it for this check:
   ```bash
   kill %1   # or: pkill -f "http.server 8080"
   ```

**Checklist (must all pass)**

- [ ] Page loads: title, metric control, chart, legend, and table are visible
- [ ] Title shows the full data range in smaller text (e.g. `2015-01 – 2026-10`);
      on mobile the range stacks under the title
- [ ] No horizontal page overflow; no JS console errors
- [ ] Chart renders series; legend toggles series on/off
- [ ] Chart zoom works (slider and/or wheel/pinch); x-axis labels densify when zoomed in
- [ ] Table “Latest” / rankings / Δ / sparklines follow the **zoom end** month
      (changing the right zoom edge updates the table; there is no separate “As of” control)
- [ ] **Compare vs** defaults to **1 year ago**; includes **1 month ago** and multi-year options;
      changing it updates rank-change, Δ, and trend without resetting zoom
- [ ] Metric switch reloads data, resets zoom to full range, and table returns to the latest month
- [ ] Rank Change column: at most `RANK_ARROW_MAX` arrows; any `+N` / `-N` is the **leftover**
      beyond those arrows (not the total move)
- [ ] Mobile table uses the card layout (no desktop thead); rank/change still readable

### What NOT to Do

- Do not add a frontend build step or JavaScript framework — the zero-tooling approach is intentional
- Do not add `unwrap()` or `expect()` in production code paths
- Do not block the tokio async executor with CPU-heavy work — use `spawn_blocking`
- Do not change the data file naming conventions without updating `pack_statistics.rs` and `index.html`

---

## Generic Instructions

<!-- This section is maintained by the repository owner. -->
<!-- Add your personal preferences, style rules, and agent behavior guidelines here. -->

### Git Policy

Never run `git commit`, `git push`, or create pull requests unless the user explicitly requests it. Always leave changes staged or unstaged for the user to review before committing.
