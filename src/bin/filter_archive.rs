//! filter_archive
//!
//! Reads an aggregated events CSV produced by `github_archive_loader`, applies
//! a configurable chain of filters, and writes the surviving rows to a new CSV
//! file.
//!
//! The filter chain in `main` is intentional: each step is a plain function
//! call so the sequence is immediately readable without digging into flags or
//! configuration.
//!
//! Usage:
//!   filter_archive --input archive-202605.csv --output archive-202605-filtered.csv
//!   filter_archive --input archive-202605.csv --output archive-202605-filtered.csv --actor-event-limit 500
//!   filter_archive --input archive-202605.csv --output archive-202605-filtered.csv --repo-issue-limit 5000
//!
//! Output: the file path specified by --output.

use anyhow::{Context, Result};
use clap::Parser;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "filter_archive",
    about = "Filter an aggregated events CSV file"
)]
struct Args {
    /// Input CSV file (actor,repo,event_type,action,language,count)
    #[arg(long)]
    input: PathBuf,

    /// Drop actors whose total event count exceeds this threshold
    #[arg(long, default_value_t = 1_000)]
    actor_event_limit: u64,

    /// Drop IssuesEvent rows for repos whose total IssuesEvent count exceeds
    /// this threshold.  Catches coordinated issue-flooding campaigns where
    /// many actors each stay below the per-actor limit.
    #[arg(long, default_value_t = 10_000)]
    repo_issue_limit: u64,

    /// Full path for the filtered output file.
    #[arg(long)]
    output: PathBuf,
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
    run(Args::parse())
}

fn run(args: Args) -> Result<()> {
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
    let rows = filter_high_volume_issue_repos(rows, args.repo_issue_limit);
    let rows = filter_issue_only_actors(rows);

    let surviving = rows.len();
    let removed_pct = if total == 0 {
        0.0
    } else {
        100.0 * (total - surviving) as f64 / total as f64
    };

    let output = &args.output;
    write_csv(rows, output)?;

    eprintln!(
        "  [total]  {:>8} removed ({:.1}%),  {:>8} remaining",
        total - surviving,
        removed_pct,
        surviving,
    );

    Ok(())
}

// ── Filters ───────────────────────────────────────────────────────────────────

/// Drops rows where the actor name contains "bot" (case-insensitive).
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
/// exclusively ForkEvents and/or WatchEvents — they never pushed, opened a
/// PR, filed an issue, or did anything else.
///
/// ForkEvent-only actors are typically users who forked a repo out of
/// curiosity and never touched it.  WatchEvent-only actors are users who
/// starred a repo.  Neither carries any language signal.
fn filter_fork_only_actors(rows: Vec<Row>) -> Vec<Row> {
    let before = rows.len();

    let actors_with_meaningful_activity: HashSet<String> = rows
        .iter()
        .filter(|r| r.event_type != "ForkEvent" && r.event_type != "WatchEvent")
        .map(|r| r.actor.clone())
        .collect();

    let rows: Vec<Row> = rows
        .into_iter()
        .filter(|r| actors_with_meaningful_activity.contains(&r.actor))
        .collect();

    log_filter("filter_fork_only_actors", before, rows.len(), "");
    rows
}

/// Drops IssuesEvent rows belonging to repos whose total IssuesEvent count
/// exceeds `limit`.
///
/// This targets coordinated issue-flooding campaigns where a large number of
/// actors each open a small number of issues against the same repo, keeping
/// every individual actor below the per-actor event limit while collectively
/// generating an extreme volume.  A real-world example from December 2023:
/// the ghscr/ghscription blockchain "inscription" protocol attracted 17 478
/// users who each opened ~22 issues in a single day (386 k IssuesEvents
/// total), causing an ~22× spike in Python's issue-count share for that month.
///
/// Only IssuesEvent rows are removed — push and PR rows for the affected repo
/// are left intact so the repo continues to contribute to push-count, pr-count,
/// and developer-activity ratings.
///
/// At the default limit of 10 000, no repos are removed in typical months
/// (the busiest legitimate issue tracker, AleoHQ/leo, peaks at ~8 200/month).
/// The limit can be tuned downward (e.g. 5 000) to also catch smaller
/// incentivised-issue campaigns at the cost of excluding that repo's issues.
fn filter_high_volume_issue_repos(rows: Vec<Row>, limit: u64) -> Vec<Row> {
    let before = rows.len();

    // Sum IssuesEvent counts per repo.
    let mut repo_issue_totals: HashMap<String, u64> = HashMap::new();
    for r in &rows {
        if r.event_type == "IssuesEvent" {
            *repo_issue_totals.entry(r.repo.clone()).or_insert(0) += r.count;
        }
    }

    let rows: Vec<Row> = rows
        .into_iter()
        .filter(|r| {
            // Only apply the cap to IssuesEvent rows; leave all other event
            // types from the same repo untouched.
            if r.event_type != "IssuesEvent" {
                return true;
            }
            repo_issue_totals.get(&r.repo).copied().unwrap_or(0) <= limit
        })
        .collect();

    log_filter(
        "filter_high_volume_issue_repos",
        before,
        rows.len(),
        &format!("limit={limit}"),
    );
    rows
}

/// Drops IssuesEvent rows from actors who have no PushEvent or PullRequestEvent
/// anywhere in the dataset.
///
/// These actors interact with repositories (filing bug reports, asking questions)
/// but never contribute code during the measured period.  They add noise to
/// issue-count ratings without reflecting developer activity.
///
/// Data from November 2023 (a normal month):
///   - ~23% of all IssuesEvents come from issue-only actors
///   - Their share is highest (58%) in repos with zero push activity and drops
///     to ~15% in actively developed repos — indicating they concentrate in
///     lower-signal, non-development contexts
///   - 97% of participants in the ghscr/ghscription inscription campaign had
///     no code activity in the same month, making this a complementary defence
///     to filter_high_volume_issue_repos
///
/// Only IssuesEvent rows are removed.  If an actor also has PushEvent or
/// PullRequestEvent rows, all their rows (including IssuesEvents) are kept.
fn filter_issue_only_actors(rows: Vec<Row>) -> Vec<Row> {
    let before = rows.len();

    // Collect actors that have at least one code event (push or PR).
    let code_actors: HashSet<String> = rows
        .iter()
        .filter(|r| r.event_type == "PushEvent" || r.event_type == "PullRequestEvent")
        .map(|r| r.actor.clone())
        .collect();

    let rows: Vec<Row> = rows
        .into_iter()
        .filter(|r| {
            // Non-issue rows are always kept regardless of actor type.
            if r.event_type != "IssuesEvent" {
                return true;
            }
            // Keep IssuesEvent rows only for actors who also write code.
            code_actors.contains(&r.actor)
        })
        .collect();

    log_filter("filter_issue_only_actors", before, rows.len(), "");
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ───────────────────────────────────────────────────────────────

    fn row(actor: &str, repo: &str, event_type: &str, count: u64) -> Row {
        Row {
            actor: actor.to_string(),
            repo: repo.to_string(),
            event_type: event_type.to_string(),
            action: String::new(),
            language: String::new(),
            count,
        }
    }

    fn actors(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.actor.as_str()).collect()
    }

    fn repos(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.repo.as_str()).collect()
    }

    // ── filter_bots ───────────────────────────────────────────────────────────

    #[test]
    fn test_filter_bots() {
        let rows = vec![
            row("alice", "a/a", "PushEvent", 1),
            row("dependabot[bot]", "a/a", "PullRequestEvent", 1),
            row("MyProjectBot", "a/a", "PushEvent", 1),
            row("BOT-ci", "a/a", "PushEvent", 1),
        ];
        let result = filter_bots(rows);
        assert_eq!(actors(&result), vec!["alice"]);
    }

    // ── filter_ci_actors ──────────────────────────────────────────────────────

    #[test]
    fn test_filter_ci_actors() {
        let rows = vec![
            row("alice", "a/a", "PushEvent", 1),
            row("github-actions[bot]", "a/a", "PushEvent", 1),
            row("Renovate-App", "a/a", "PullRequestEvent", 1),  // case-insensitive
            row("snyk-io", "a/a", "PushEvent", 1),
            row("netlify[bot]", "a/a", "PushEvent", 1),
        ];
        let result = filter_ci_actors(rows);
        assert_eq!(actors(&result), vec!["alice"]);
    }

    // ── filter_single_event_repos ─────────────────────────────────────────────

    #[test]
    fn test_filter_single_event_repos() {
        let rows = vec![
            row("alice", "active/repo", "PushEvent", 3),
            row("bob", "active/repo", "PushEvent", 1),   // total=4, kept
            row("carol", "quiet/repo", "PushEvent", 1),  // total=1, dropped
        ];
        let result = filter_single_event_repos(rows);
        assert_eq!(repos(&result), vec!["active/repo", "active/repo"]);
    }

    // ── filter_empty_repos ────────────────────────────────────────────────────

    #[test]
    fn test_filter_empty_repos() {
        let rows = vec![
            row("alice", "code/repo", "PushEvent", 2),
            row("alice", "code/repo", "IssuesEvent", 1),   // kept: repo has pushes
            row("bob", "watch/repo", "WatchEvent", 5),     // dropped: no push/PR
            row("bob", "watch/repo", "IssuesEvent", 1),    // dropped: same repo
        ];
        let result = filter_empty_repos(rows);
        assert_eq!(repos(&result), vec!["code/repo", "code/repo"]);
    }

    // ── filter_high_volume_actors ─────────────────────────────────────────────

    #[test]
    fn test_filter_high_volume_actors() {
        let rows = vec![
            row("alice", "a/a", "PushEvent", 5),
            row("alice", "a/b", "PushEvent", 5),   // alice total=10, kept (limit=10)
            row("bob", "b/a", "PushEvent", 11),     // bob total=11, dropped
        ];
        let result = filter_high_volume_actors(rows, 10);
        assert_eq!(actors(&result), vec!["alice", "alice"]);
    }

    // ── filter_deleted_repos ──────────────────────────────────────────────────

    #[test]
    fn test_filter_deleted_repos() {
        let sha = "a".repeat(40);
        let uuid = "550e8400-e29b-41d4-a716-446655440000";
        let rows = vec![
            row("alice", "normal/repo", "PushEvent", 1),
            row("alice", &format!("{sha}/repo"), "PushEvent", 1),
            row("alice", &format!("owner/{sha}"), "PushEvent", 1),
            row("alice", &format!("{uuid}/repo"), "PushEvent", 1),
            row("alice", &format!("owner/{uuid}"), "PushEvent", 1),
        ];
        let result = filter_deleted_repos(rows);
        assert_eq!(repos(&result), vec!["normal/repo"]);
    }

    // ── filter_fork_only_actors ───────────────────────────────────────────────

    #[test]
    fn test_filter_fork_only_actors() {
        let rows = vec![
            row("alice", "a/a", "PushEvent", 1),     // alice has code activity → kept
            row("alice", "a/a", "ForkEvent", 1),     // kept: alice isn't fork-only
            row("bob", "b/b", "ForkEvent", 3),        // bob is fork-only → dropped
            row("carol", "c/c", "WatchEvent", 2),    // carol is watch-only → dropped
        ];
        let result = filter_fork_only_actors(rows);
        assert_eq!(actors(&result), vec!["alice", "alice"]);
    }

    // ── filter_high_volume_issue_repos ────────────────────────────────────────

    #[test]
    fn test_filter_high_volume_issue_repos() {
        let rows = vec![
            row("alice", "spam/repo", "IssuesEvent", 200),  // total issues=200 > limit=100
            row("alice", "spam/repo", "PushEvent", 1),       // push kept even for spam/repo
            row("bob", "good/repo", "IssuesEvent", 50),      // total issues=50, kept
        ];
        let result = filter_high_volume_issue_repos(rows, 100);
        assert_eq!(
            result.iter().map(|r| (r.repo.as_str(), r.event_type.as_str())).collect::<Vec<_>>(),
            vec![("spam/repo", "PushEvent"), ("good/repo", "IssuesEvent")]
        );
    }

    // ── filter_issue_only_actors ──────────────────────────────────────────────

    #[test]
    fn test_filter_issue_only_actors() {
        let rows = vec![
            row("alice", "a/a", "PushEvent", 1),      // alice has code → issue row kept
            row("alice", "a/a", "IssuesEvent", 2),
            row("bob", "b/b", "IssuesEvent", 5),       // bob has no code → issue row dropped
        ];
        let result = filter_issue_only_actors(rows);
        assert_eq!(
            result.iter().map(|r| r.actor.as_str()).collect::<Vec<_>>(),
            vec!["alice", "alice"]
        );
    }

    // ── end-to-end via run() ──────────────────────────────────────────────────

    #[test]
    fn test_filter_archive_end_to_end() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path();

        std::fs::write(
            dir.join("archive-202401.csv"),
            r#"actor,repo,event_type,action,language,count
alice,rust-lang/rust,PushEvent,,,5
alice,rust-lang/rust,PullRequestEvent,opened,,3
alice,rust-lang/rust,IssuesEvent,opened,,2
bob,rust-lang/rust,PushEvent,,,2
dependabot[bot],some/repo,PullRequestEvent,opened,,1
carol,single-event/repo,PushEvent,,,1
dave,golang/go,WatchEvent,,,10
eve,spam/repo,IssuesEvent,opened,,20000
eve,spam/repo,PushEvent,,,1
"#,
        )?;

        run(Args {
            input: dir.join("archive-202401.csv"),
            output: dir.join("archive-202401-filtered.csv"),
            actor_event_limit: 1_000,
            repo_issue_limit: 10_000,
        })?;

        assert_eq!(
            std::fs::read_to_string(dir.join("archive-202401-filtered.csv"))?,
            r#"actor,repo,event_type,action,language,count
alice,rust-lang/rust,PushEvent,,,5
alice,rust-lang/rust,PullRequestEvent,opened,,3
alice,rust-lang/rust,IssuesEvent,opened,,2
bob,rust-lang/rust,PushEvent,,,2
"#
        );

        Ok(())
    }
}

