# AGENTS.md

## Project-Specific Instructions

### What This Project Is

**githubstats** is a data pipeline and interactive visualization for GitHub programming language popularity, derived from real developer activity on [GH Archive](https://www.gharchive.org/).

The pipeline downloads every public GitHub event for a given month, filters out bots and automated noise, resolves language compositions per repository, and computes multiple weighted language activity ratings. The results are consumed by a single-page web UI (`index.html`) that renders an interactive trend chart (via Apache ECharts) and a ranked table with sparklines.

The project covers January 2015 through the present (~11 years of monthly data). A key data discontinuity exists: GitHub removed language data from event payloads in October 2025. Before that date, language attribution uses the single primary language field; from October 2025 onward, the GraphQL API is required for full multi-language breakdown.

### Technology Stack

- **Backend:** Rust (Edition 2024), async with `tokio`, HTTP with `reqwest`, JSON with `serde_json`, CLI with `clap`
- **Frontend:** Pure HTML + CSS + vanilla JavaScript, no build step; Apache ECharts 5 loaded from CDN
- **CI:** GitHub Actions — `cargo fmt --check` + `cargo clippy -D warnings` on every push/PR

### Project Structure

```
githubstats/
├── src/bin/                    # Five standalone CLI binaries (the entire backend)
│   ├── github_archive_loader.rs   # Step 1: GH Archive → aggregated CSV
│   ├── filter_archive.rs          # Step 2: filtered CSV (bot/noise removal)
│   ├── github_language_loader.rs  # Step 3: repo slugs → GitHub GraphQL → language JSONL
│   ├── produce_statistics.rs      # Step 4: CSV + languages → 5 per-month rating files
│   └── pack_statistics.rs         # Step 5: per-month files → 5 combined all-months files
├── data/                       # All intermediate and final data (83 GB, partially gitignored)
│   ├── archive-YYYYMM-filtered.csv           # Gitignored; ~0.1–1 GB each, 138 months
│   ├── languages-YYYY-MM.jsonl               # Gitignored; repo → language breakdown per month
│   ├── language-ratings-YYYY-MM-<type>.jsonl # Per-month ratings, one file per type/month
│   └── language-ratings-all-<type>.jsonl     # Combined files consumed by the frontend
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
| Raw archive (gitignored) | `data/archive-YYYYMM.csv` |
| Filtered archive (gitignored) | `data/archive-YYYYMM-filtered.csv` |
| Language lookup (gitignored) | `data/languages-YYYY-MM.jsonl` |
| Per-month rating (committed) | `data/language-ratings-YYYY-MM-<type>.jsonl` |
| Combined all-months rating (committed) | `data/language-ratings-all-<type>.jsonl` |

**Valid `<type>` values:** `pr-count`, `issue-count`, `push-count`, `developer-activity`, `active-repos`, `star-count`

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

**Staged concurrent pipeline (`github_archive_loader`):** Uses bounded `async-channel` MPMC channels between stages. Network I/O is async; CPU-intensive work (gzip decompression, JSON parsing) runs on `tokio::task::spawn_blocking`. This pattern must be preserved to avoid blocking the async executor.

**stderr for diagnostics, stdout for data (`github_language_loader`):** All progress output goes to `stderr`; all data output goes to `stdout` as clean JSONL. This enables shell piping and must be maintained for any binary that reads/writes data streams.

**Sequential filter chain (`filter_archive`):** Each filter is a pure function `Vec<Row> → Vec<Row>` named `filter_<noun>`. The `main` function reads as a pipeline. Follow this pattern when adding new filters. Every filter must log a `[filter_name] N removed (X.X%), M remaining` line to stderr.

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
| `GITHUB_TOKEN` | `github_language_loader` | GitHub PAT for GraphQL API (public repo read access sufficient) |

### Frontend Testing

After making any change to `index.html`, verify it visually using the integrated browser tool:

1. Start a local HTTP server in the background (do not use `file://` URLs — ECharts and fetch-based data loading require HTTP):
   ```bash
   python3 -m http.server 8080 --directory . &
   ```
2. Navigate the browser tool to `http://localhost:8080/index.html`.
3. Take a snapshot or screenshot and confirm the page renders correctly (chart visible, table populated, no console errors).
4. Check the browser console for JavaScript errors (`playwright_browser_console_messages` at level `error`).
5. Kill the server when done:
   ```bash
   kill %1   # or: pkill -f "http.server 8080"
   ```

If port 8080 is already in use, try 8081, 8082, etc.

### What NOT to Do

- Do not add a frontend build step or JavaScript framework — the zero-tooling approach is intentional
- Do not add `unwrap()` or `expect()` in production code paths
- Do not block the tokio async executor with CPU-heavy work — use `spawn_blocking`
- Do not change the data file naming conventions without updating `pack_statistics.rs` and `index.html`

---

## Generic Instructions

<!-- This section is maintained by the repository owner. -->
<!-- Add your personal preferences, style rules, and agent behavior guidelines here. -->
