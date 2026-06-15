//! filter_csv
//!
//! Reads an aggregated events CSV produced by `github_archive_loader`, applies
//! a configurable chain of filters, and writes the surviving rows to a new CSV
//! file with a `-filtered` suffix.
//!
//! The filter chain in `main` is intentional: each step is a plain function
//! call so the sequence is immediately readable without digging into flags or
//! configuration.
//!
//! Current filters (applied in order):
//!
//!   filter_bots               — drops rows where the actor name contains "bot"
//!                               (case-insensitive).
//!
//!   filter_ci_actors          — drops rows whose actor name matches known CI /
//!                               automation tools not caught by "bot" alone
//!                               (e.g. dependabot, renovate, github-actions, …).
//!
//!   filter_high_volume_actors — drops rows belonging to actors whose total
//!                               event count across the whole file exceeds a
//!                               threshold (default: 1 000).
//!
//!   filter_deleted_repos      — drops rows whose repo name looks like an
//!                               auto-generated placeholder left by a deleted
//!                               account (owner or name is a raw git SHA or UUID).
//!
//!   filter_empty_repos        — drops rows belonging to repos that have no
//!                               PushEvent or PullRequestEvent in the dataset.
//!                               These repos have no code activity and carry no
//!                               language signal.
//!
//!   filter_fork_only_actors   — drops rows belonging to actors whose entire
//!                               activity in the dataset consists solely of
//!                               ForkEvents.
//!
//!   filter_single_event_repos — drops rows belonging to repos whose total
//!                               event count across all actors is exactly 1.
//!                               The vast majority are one-off noise with no
//!                               analytical value.
//!
//! Usage:
//!   filter_csv --input archive-202605.csv
//!   filter_csv --input archive-202605.csv --actor-event-limit 500
//!
//! Output: same directory as input, filename with `-filtered` inserted before
//! the extension, e.g. `archive-202605-filtered.csv`.

use anyhow::{Context, Result};
use clap::Parser;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "filter_csv", about = "Filter an aggregated events CSV file")]
struct Args {
    /// Input CSV file (actor,repo,event_type,action,language,count)
    #[arg(long)]
    input: PathBuf,

    /// Drop actors whose total event count exceeds this threshold
    #[arg(long, default_value_t = 1_000)]
    actor_event_limit: u64,
}

// ── Data model ────────────────────────────────────────────────────────────────

struct Row {
    actor: String,
    repo: String,
    event_type: String,
    action: String,
    language: String,
    count: u64,
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let args = Args::parse();

    let rows = read_csv(&args.input)?;
    let total = rows.len();
    eprintln!("  [read]                         {:>8} rows", total);

    let rows = filter_empty_repos(rows);
    let rows = filter_bots(rows);
    let rows = filter_ci_actors(rows);
    let rows = filter_single_event_repos(rows);
    let rows = filter_high_volume_actors(rows, args.actor_event_limit);
    let rows = filter_deleted_repos(rows);
    let rows = filter_fork_only_actors(rows);

    let surviving = rows.len();
    let removed_pct = if total == 0 { 0.0 } else { 100.0 * (total - surviving) as f64 / total as f64 };

    let output = output_path(&args.input)?;
    write_csv(rows, &output)?;

    eprintln!(
        "  [total]  {:>8} removed ({:.1}%),  {:>8} remaining",
        total - surviving, removed_pct, surviving,
    );

    Ok(())
}

// ── Filters ───────────────────────────────────────────────────────────────────

/// Drops rows where the actor name contains "bot" (case-insensitive).
/// Catches common patterns like `dependabot`, `github-actions[bot]`,
/// `renovate[bot]`, `someproject-bot`, etc.
fn filter_bots(rows: Vec<Row>) -> Vec<Row> {
    let before = rows.len();
    let rows: Vec<Row> = rows
        .into_iter()
        .filter(|r| !r.actor.to_ascii_lowercase().contains("bot"))
        .collect();
    log_filter("filter_bots", before, rows.len(), "");
    rows
}

/// Drops rows whose actor name matches known CI / automation tools that do not
/// happen to contain "bot" in their name.
///
/// Matching is case-insensitive substring, so e.g. "github-actions" catches
/// both `github-actions` and `github-actions[bot]` (the latter also caught by
/// `filter_bots`, but the overlap is harmless).
fn filter_ci_actors(rows: Vec<Row>) -> Vec<Row> {
    const CI_SUBSTRINGS: &[&str] = &[
        "github-actions",
        "dependabot",
        "renovate",
        "codecov",
        "snyk",
        "deepsource",
        "sonarcloud",
        "greenkeeper",
        "imgbot",
        "allcontributors",
        "semantic-release",
        "release-please",
        "pre-commit-ci",
        "lgtm-com",
        "netlify",
        "vercel",
        "stale[",
        "copilot",
    ];

    let before = rows.len();
    let rows: Vec<Row> = rows
        .into_iter()
        .filter(|r| {
            let actor_lc = r.actor.to_ascii_lowercase();
            !CI_SUBSTRINGS.iter().any(|s| actor_lc.contains(s))
        })
        .collect();
    log_filter("filter_ci_actors", before, rows.len(), "");
    rows
}

/// Drops all rows belonging to actors whose total event count (sum of the
/// `count` column across all their rows) exceeds `limit`.
/// These are typically CI systems, mirror scripts, or automated pipelines
/// that are not real developer activity.
fn filter_high_volume_actors(rows: Vec<Row>, limit: u64) -> Vec<Row> {
    let before = rows.len();

    let mut actor_totals: HashMap<String, u64> = HashMap::new();
    for r in &rows {
        *actor_totals.entry(r.actor.clone()).or_insert(0) += r.count;
    }

    let rows: Vec<Row> = rows
        .into_iter()
        .filter(|r| actor_totals[&r.actor] <= limit)
        .collect();

    log_filter(
        "filter_high_volume_actors",
        before,
        rows.len(),
        &format!("limit={limit}"),
    );
    rows
}

/// Drops rows whose repo name looks like a deleted-account placeholder.
///
/// When a GitHub account is deleted, repos it owned are sometimes renamed to
/// a raw SHA-like or UUID-like slug internally. We detect:
///   - owner or repo-name segment that is a 40-character hex string (git SHA)
///   - owner or repo-name segment that matches a UUID (8-4-4-4-12 hex)
fn filter_deleted_repos(rows: Vec<Row>) -> Vec<Row> {
    let before = rows.len();
    let rows: Vec<Row> = rows
        .into_iter()
        .filter(|r| !looks_like_deleted_repo(&r.repo))
        .collect();
    log_filter("filter_deleted_repos", before, rows.len(), "");
    rows
}

fn looks_like_deleted_repo(repo: &str) -> bool {
    let Some((owner, name)) = repo.split_once('/') else {
        return false;
    };
    is_sha_like(owner) || is_sha_like(name) || is_uuid_like(owner) || is_uuid_like(name)
}

/// Returns true for 40-character lowercase hex strings (git SHA1).
fn is_sha_like(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Returns true for strings matching the UUID format (8-4-4-4-12 hex).
fn is_uuid_like(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    let is_hex = |c: u8| c.is_ascii_hexdigit();
    b[8] == b'-'
        && b[13] == b'-'
        && b[18] == b'-'
        && b[23] == b'-'
        && b[..8].iter().all(|&c| is_hex(c))
        && b[9..13].iter().all(|&c| is_hex(c))
        && b[14..18].iter().all(|&c| is_hex(c))
        && b[19..23].iter().all(|&c| is_hex(c))
        && b[24..].iter().all(|&c| is_hex(c))
}

/// Drops rows belonging to repos that have no PushEvent or PullRequestEvent
/// anywhere in the dataset.
///
/// Repos with only WatchEvents, ForkEvents, IssuesEvents, etc. and no code
/// activity carry no language signal and are typically empty or archived repos.
fn filter_empty_repos(rows: Vec<Row>) -> Vec<Row> {
    let before = rows.len();

    let active_repos: HashSet<String> = rows
        .iter()
        .filter(|r| r.event_type == "PushEvent" || r.event_type == "PullRequestEvent")
        .map(|r| r.repo.clone())
        .collect();

    let rows: Vec<Row> = rows
        .into_iter()
        .filter(|r| active_repos.contains(&r.repo))
        .collect();

    log_filter("filter_empty_repos", before, rows.len(), "");
    rows
}

/// Drops rows belonging to actors whose entire activity in the dataset is
/// exclusively ForkEvents — they never pushed, opened a PR, filed an issue,
/// or did anything else.
///
/// These are typically users who forked a repo out of curiosity and never
/// touched it; they add no signal to language or activity analysis.
fn filter_fork_only_actors(rows: Vec<Row>) -> Vec<Row> {
    let before = rows.len();

    let actors_with_non_fork: HashSet<String> = rows
        .iter()
        .filter(|r| r.event_type != "ForkEvent")
        .map(|r| r.actor.clone())
        .collect();

    let rows: Vec<Row> = rows
        .into_iter()
        .filter(|r| actors_with_non_fork.contains(&r.actor))
        .collect();

    log_filter("filter_fork_only_actors", before, rows.len(), "");
    rows
}

/// Drops rows belonging to repos whose total event count (sum of `count`
/// across all actors and event types) is exactly 1.
///
/// These one-off repos make up ~56% of all rows but contribute negligible
/// signal — a single event from an unknown repo tells us nothing useful about
/// language trends.
fn filter_single_event_repos(rows: Vec<Row>) -> Vec<Row> {
    let before = rows.len();

    let mut repo_totals: HashMap<String, u64> = HashMap::new();
    for r in &rows {
        *repo_totals.entry(r.repo.clone()).or_insert(0) += r.count;
    }

    let rows: Vec<Row> = rows
        .into_iter()
        .filter(|r| repo_totals[&r.repo] > 1)
        .collect();

    log_filter("filter_single_event_repos", before, rows.len(), "");
    rows
}

// ── Logging helper ────────────────────────────────────────────────────────────

fn log_filter(name: &str, before: usize, after: usize, note: &str) {
    let removed = before - after;
    let removed_pct = if before == 0 {
        0.0
    } else {
        100.0 * removed as f64 / before as f64
    };
    let note_str = if note.is_empty() {
        String::new()
    } else {
        format!("  ({note})")
    };
    eprintln!(
        "  [{name:<30}]  {:>8} removed ({:4.1}%),  {:>8} remaining{note_str}",
        removed, removed_pct, after,
    );
}

// ── I/O ───────────────────────────────────────────────────────────────────────

/// Reads the CSV into a `Vec<Row>`, skipping the header and any malformed lines.
fn read_csv(path: &Path) -> Result<Vec<Row>> {
    let file = File::open(path).with_context(|| format!("open {path:?}"))?;
    let reader = BufReader::with_capacity(1 << 20, file);
    let mut lines = reader.lines();

    // Consume and validate header
    let header = lines.next().with_context(|| "file is empty")??;
    if !header.starts_with("actor,") {
        eprintln!("WARN: unexpected header: {header}");
    }

    let mut rows = Vec::new();
    let mut bad = 0u64;

    for line_result in lines {
        let line = line_result.context("I/O error reading line")?;
        if line.is_empty() {
            continue;
        }
        let fields = split_csv_line(&line);
        if fields.len() != 6 {
            bad += 1;
            continue;
        }
        let count: u64 = match fields[5].trim().parse() {
            Ok(n) => n,
            Err(_) => {
                bad += 1;
                continue;
            }
        };
        rows.push(Row {
            actor: fields[0].clone(),
            repo: fields[1].clone(),
            event_type: fields[2].clone(),
            action: fields[3].clone(),
            language: fields[4].clone(),
            count,
        });
    }

    if bad > 0 {
        eprintln!("WARN: {bad} malformed rows skipped");
    }

    Ok(rows)
}

/// Writes rows as RFC 4180 CSV.
fn write_csv(rows: Vec<Row>, path: &Path) -> Result<()> {
    let file = File::create(path).with_context(|| format!("create {path:?}"))?;
    let mut w = BufWriter::new(file);
    w.write_all(b"actor,repo,event_type,action,language,count\n")
        .context("write header")?;

    for r in &rows {
        let actor = csv_field(&r.actor);
        let repo = csv_field(&r.repo);
        let event_type = csv_field(&r.event_type);
        let action = csv_field(&r.action);
        let language = csv_field(&r.language);
        writeln!(
            w,
            "{actor},{repo},{event_type},{action},{language},{}",
            r.count
        )
        .context("write row")?;
    }

    w.flush().context("flush")?;
    eprintln!(
        "  [write]                        {:>8} rows written  →  {path:?}",
        rows.len()
    );
    Ok(())
}

/// Derives the output path by inserting `-filtered` before the file extension.
/// `archive-202605.csv` → `archive-202605-filtered.csv`
fn output_path(input: &Path) -> Result<PathBuf> {
    let stem = input
        .file_stem()
        .with_context(|| "input has no file stem")?
        .to_string_lossy();
    let ext = input
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let filename = format!("{stem}-filtered{ext}");
    Ok(input.with_file_name(filename))
}

// ── CSV helpers ───────────────────────────────────────────────────────────────

/// Quotes a CSV field if it contains a comma, double-quote, or newline.
fn csv_field(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains([',', '"', '\n', '\r']) {
        std::borrow::Cow::Owned(format!("\"{}\"", s.replace('"', "\"\"")))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// Minimal RFC 4180 CSV line splitter.
fn split_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::with_capacity(6);
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes => {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            }
            '"' => in_quotes = true,
            ',' if !in_quotes => {
                fields.push(field.clone());
                field.clear();
            }
            other => field.push(other),
        }
    }
    fields.push(field);
    fields
}
