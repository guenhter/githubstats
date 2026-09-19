//! produce-statistics
//!
//! Joins an events CSV file (output of `event_loader` / `filter_events`)
//! with a repo-languages JSONL file and produces multiple language-rating files
//! in the specified output directory.
//!
//! Output files (JSONL, sorted descending by rating):
//!   language-ratings-YYYY-MM-pr-count.jsonl
//!   language-ratings-YYYY-MM-issue-count.jsonl
//!   language-ratings-YYYY-MM-push-count.jsonl
//!   language-ratings-YYYY-MM-active-repos.jsonl
//!   language-ratings-YYYY-MM-star-count.jsonl
//!
//! Rating formula (all metric types):
//!   For each repo, distribute the event count across all its languages
//!   weighted by byte share.
//!   pr-count:     rating[L] += pr_count    × (size_L / total_size)
//!   issue-count:  rating[L] += issue_count × (size_L / total_size)
//!   push-count:   rating[L] += push_count  × (size_L / total_size)
//!   active-repos: rating[L] += 1           × (size_L / total_size)
//!                 (once per repo that had any PushEvent or PullRequestEvent)
//!   star-count:   rating[L] += star_count  × (size_L / total_size)
//!
//! Optional `--cap-single-actor-events`:
//!   For repos with exactly one distinct actor across PushEvent + PullRequestEvent,
//!   contribute at most 1 to pr-count and at most 1 to push-count.  Multi-actor
//!   repos keep full volume.  active-repos / issues / stars are unchanged.
//!   Blunts single-person push mills without dropping those repos.
//!
//! Event types read from the events CSV:
//!   PullRequestEvent → pr-count and active-repos
//!   IssuesEvent      → issue-count
//!   PushEvent        → push-count and active-repos
//!   WatchEvent       → star-count
//!
//! Input formats:
//!   --events        CSV: actor,repo,event_type,action,language,count
//!   --repo-languages JSONL: {"repo":"…","total_size":N,"languages":[{"language":"Rust","size":N},…]}
//!
//! The YEAR and MONTH for the output filename are inferred from the events
//! filename (`events-YYYY-MM.csv`).
//!
//! All progress and diagnostic messages go to stderr.

use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

// ── Type aliases ──────────────────────────────────────────────────────────────

/// Per-repo language breakdown: (total_size_bytes, [(language, size_bytes)])
/// ordered by size descending (first entry = primary language).
type LangMap = HashMap<String, (u64, Vec<(String, u64)>)>;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "produce-statistics",
    about = "Compute weighted language ratings from an events CSV and repo language breakdowns.\n\
             Produces multiple JSONL files in --output-dir, one per statistic type."
)]
struct Args {
    /// Events CSV file produced by event_loader / filter_events.
    /// Format: actor,repo,event_type,action,language,count
    /// The filename must be `events-YYYY-MM.csv`.
    #[arg(long)]
    events: PathBuf,

    /// JSONL file with per-repo language breakdowns.
    /// Format: {"repo":"owner/repo","total_size":158498874,"languages":[{"language":"Rust","size":143102371},…]}
    #[arg(long = "repo-languages")]
    repo_languages: PathBuf,

    /// Directory where the output JSONL files will be written.
    /// Files are named: language-ratings-YYYY-MM-<type>.jsonl
    #[arg(long)]
    output_dir: PathBuf,

    /// Cap pr-count and push-count at 1 for single-actor repos (exactly one
    /// distinct actor across PushEvent + PullRequestEvent this month).
    #[arg(long, default_value_t = false)]
    cap_single_actor_events: bool,
}

// ── Data types ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ProjectLanguages {
    repo: String,
    total_size: u64,
    languages: Vec<LanguageEntry>,
}

#[derive(Deserialize)]
struct LanguageEntry {
    language: String,
    size: u64,
}

/// All per-repo activity counts collected from the events CSV in a single pass.
struct RepoCounts {
    /// Total PullRequestEvent count per repo.
    pr_counts: HashMap<String, u64>,
    /// Distinct actors that generated a PullRequestEvent or PushEvent, per repo.
    /// Used by `--cap-single-actor-events`, not as a published rating.
    dev_actors: HashMap<String, usize>,
    /// Total IssuesEvent count per repo.
    issue_counts: HashMap<String, u64>,
    /// Total PushEvent count per repo.
    push_counts: HashMap<String, u64>,
    /// Repos that had at least one PushEvent or PullRequestEvent (value is always 1).
    active_repos: HashMap<String, u64>,
    /// Total WatchEvent (star) count per repo.
    star_counts: HashMap<String, u64>,
}

// ── Main ─────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    run(Args::parse())
}

fn run(args: Args) -> Result<()> {
    // Infer YYYY-MM from the events filename.
    let year_month = infer_year_month(&args.events)?;
    eprintln!("Inferred period: {year_month}");

    // Create output directory if it doesn't exist.
    std::fs::create_dir_all(&args.output_dir)
        .with_context(|| format!("cannot create output dir {:?}", args.output_dir))?;

    eprintln!("Loading languages from {:?} …", args.repo_languages);
    let lang_map = load_languages(&args.repo_languages)?;
    eprintln!("  {} repos with language data", lang_map.len());

    eprintln!("Reading activity from {:?} …", args.events);
    let mut counts = collect_counts(&args.events)?;
    if args.cap_single_actor_events {
        let n = cap_single_actor_events(&mut counts);
        eprintln!("  [cap_single_actor_events] capped push/PR to 1 on {n} single-actor repos");
    }
    eprintln!(
        "  {} repos with PR activity, {} with issue activity, {} with push activity, {} active repos, {} with star activity",
        counts.pr_counts.len(),
        counts.issue_counts.len(),
        counts.push_counts.len(),
        counts.active_repos.len(),
        counts.star_counts.len(),
    );

    // ── pr-count ─────────────────────────────────────────────────────────────
    let out = output_path(&args.output_dir, &year_month, "pr-count");
    eprintln!("Writing {out:?} …");
    {
        let mut w = open_writer(&out)?;
        let ratings = compute_ratings(&counts.pr_counts, &lang_map, "PR");
        write_ratings(&mut w, &ratings)?;
    }

    // ── issue-count ──────────────────────────────────────────────────────────
    let out = output_path(&args.output_dir, &year_month, "issue-count");
    eprintln!("Writing {out:?} …");
    {
        let mut w = open_writer(&out)?;
        let ratings = compute_ratings(&counts.issue_counts, &lang_map, "issue");
        write_ratings(&mut w, &ratings)?;
    }

    // ── push-count ───────────────────────────────────────────────────────────
    let out = output_path(&args.output_dir, &year_month, "push-count");
    eprintln!("Writing {out:?} …");
    {
        let mut w = open_writer(&out)?;
        let ratings = compute_ratings(&counts.push_counts, &lang_map, "push");
        write_ratings(&mut w, &ratings)?;
    }

    // ── active-repos ─────────────────────────────────────────────────────────
    let out = output_path(&args.output_dir, &year_month, "active-repos");
    eprintln!("Writing {out:?} …");
    {
        let mut w = open_writer(&out)?;
        let ratings = compute_ratings(&counts.active_repos, &lang_map, "active-repos");
        write_ratings(&mut w, &ratings)?;
    }

    // ── star-count ───────────────────────────────────────────────────────────
    let out = output_path(&args.output_dir, &year_month, "star-count");
    eprintln!("Writing {out:?} …");
    {
        let mut w = open_writer(&out)?;
        let ratings = compute_ratings(&counts.star_counts, &lang_map, "star-count");
        write_ratings(&mut w, &ratings)?;
    }

    eprintln!("Done.");
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract `YYYY-MM` from `events-YYYY-MM.csv`.
fn infer_year_month(path: &PathBuf) -> Result<String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("cannot read filename from {:?}", path))?;

    name.strip_prefix("events-")
        .and_then(|s| s.strip_suffix(".csv"))
        .map(str::to_owned)
        .with_context(|| format!("expected events-YYYY-MM.csv, got {name:?}"))
}

/// Build the output file path for a given type.
fn output_path(dir: &Path, year_month: &str, kind: &str) -> PathBuf {
    dir.join(format!("language-ratings-{year_month}-{kind}.jsonl"))
}

/// Open a file for writing, wrapped in a BufWriter.
fn open_writer(path: &PathBuf) -> Result<BufWriter<File>> {
    File::create(path)
        .with_context(|| format!("cannot create {:?}", path))
        .map(BufWriter::new)
}

/// Write a sorted-descending list of (language, rating) pairs as JSONL.
/// Each record includes the rating and its percentage share of the total.
fn write_ratings(w: &mut impl Write, ratings: &[(String, f64)]) -> Result<()> {
    let total: f64 = ratings.iter().map(|(_, r)| r).sum();
    for (language, rating) in ratings {
        let rating = (rating * 100.0).round() / 100.0;
        let percentage = if total > 0.0 {
            (rating / total * 10000.0).round() / 100.0
        } else {
            0.0
        };
        serde_json::to_writer(
            &mut *w,
            &json!({"language": language, "rating": rating, "percentage": percentage}),
        )
        .context("serialise")?;
        w.write_all(b"\n")?;
    }
    Ok(())
}

// ── I/O helpers ───────────────────────────────────────────────────────────────

/// Load the languages JSONL into a map keyed by repo slug.
/// Value is (total_size, [(language, size)]) ordered by size descending
/// (so the first entry is always the primary language).
fn load_languages(path: &PathBuf) -> Result<LangMap> {
    let reader = open(path)?;
    let mut map: HashMap<String, (u64, Vec<(String, u64)>)> = HashMap::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line.context("read error")?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<ProjectLanguages>(line) {
            Ok(pl) => {
                map.insert(
                    pl.repo,
                    (
                        pl.total_size,
                        pl.languages
                            .into_iter()
                            .map(|e| (e.language, e.size))
                            .collect(),
                    ),
                );
            }
            Err(e) => eprintln!("  [skip] languages line {}: {e}", i + 1),
        }
    }
    Ok(map)
}

/// Read the events CSV in a single pass and accumulate counts for all
/// relevant event types.
///
/// CSV format (first row is header):
///   actor,repo,event_type,action,language,count
fn collect_counts(path: &PathBuf) -> Result<RepoCounts> {
    let file = File::open(path).with_context(|| format!("cannot open {path:?}"))?;
    let mut rdr = csv::Reader::from_reader(file);

    let mut pr_counts: HashMap<String, u64> = HashMap::new();
    let mut dev_actor_sets: HashMap<String, HashSet<String>> = HashMap::new();
    let mut issue_counts: HashMap<String, u64> = HashMap::new();
    let mut push_counts: HashMap<String, u64> = HashMap::new();
    let mut active_repo_set: HashSet<String> = HashSet::new();
    let mut star_counts: HashMap<String, u64> = HashMap::new();
    let mut parse_errors = 0u64;

    for (i, result) in rdr.records().enumerate() {
        let record = match result {
            Ok(r) => r,
            Err(e) => {
                eprintln!("  [skip] CSV record {}: {e}", i + 2);
                parse_errors += 1;
                continue;
            }
        };
        if record.len() < 6 {
            eprintln!(
                "  [skip] CSV record {}: expected 6 fields, got {}",
                i + 2,
                record.len()
            );
            parse_errors += 1;
            continue;
        }
        let actor = &record[0];
        let repo = &record[1];
        let event_type = &record[2];
        let count_str = &record[5];

        let count: u64 = match count_str.parse() {
            Ok(v) => v,
            Err(_) => {
                eprintln!("  [skip] non-numeric count on CSV record {}", i + 2);
                parse_errors += 1;
                continue;
            }
        };

        match event_type {
            "PullRequestEvent" => {
                *pr_counts.entry(repo.to_string()).or_insert(0) += count;
                dev_actor_sets
                    .entry(repo.to_string())
                    .or_default()
                    .insert(actor.to_string());
                active_repo_set.insert(repo.to_string());
            }
            "IssuesEvent" => {
                *issue_counts.entry(repo.to_string()).or_insert(0) += count;
            }
            "PushEvent" => {
                *push_counts.entry(repo.to_string()).or_insert(0) += count;
                dev_actor_sets
                    .entry(repo.to_string())
                    .or_default()
                    .insert(actor.to_string());
                active_repo_set.insert(repo.to_string());
            }
            "WatchEvent" => {
                *star_counts.entry(repo.to_string()).or_insert(0) += count;
            }
            _ => {}
        }
    }

    if parse_errors > 0 {
        eprintln!("  ({parse_errors} parse errors)");
    }

    let dev_actors: HashMap<String, usize> = dev_actor_sets
        .into_iter()
        .map(|(repo, actors)| (repo, actors.len()))
        .collect();

    // Each active repo contributes exactly 1 (regardless of event count).
    let active_repos: HashMap<String, u64> = active_repo_set
        .into_iter()
        .map(|repo| (repo, 1u64))
        .collect();

    Ok(RepoCounts {
        pr_counts,
        dev_actors,
        issue_counts,
        push_counts,
        active_repos,
        star_counts,
    })
}

/// Scoring companion to `filter_events`'s volume caps: for repos with exactly
/// one distinct actor (push + PR), set push-count and pr-count to at most 1.
/// Keeps the repos in the dataset but removes remaining push-mill volume from
/// the ratings. Returns how many repos were capped.
fn cap_single_actor_events(counts: &mut RepoCounts) -> usize {
    let mut capped = 0usize;
    for (repo, n_actors) in &counts.dev_actors {
        if *n_actors != 1 {
            continue;
        }
        let mut changed = false;
        if let Some(c) = counts.push_counts.get_mut(repo)
            && *c > 1
        {
            *c = 1;
            changed = true;
        }
        if let Some(c) = counts.pr_counts.get_mut(repo)
            && *c > 1
        {
            *c = 1;
            changed = true;
        }
        if changed {
            capped += 1;
        }
    }
    capped
}

/// Compute language ratings from a map of per-repo event counts.
///
/// For each repo, distributes the event count across all its languages
/// weighted by byte share (proportional attribution).
fn compute_ratings(
    repo_event_counts: &HashMap<String, u64>,
    lang_map: &LangMap,
    label: &str,
) -> Vec<(String, f64)> {
    let mut ratings: HashMap<String, f64> = HashMap::new();
    let mut matched = 0u64;
    let mut unmatched = 0u64;

    for (repo, count) in repo_event_counts {
        if let Some((total_size, langs)) = lang_map.get(repo.as_str()) {
            if *total_size > 0 {
                for (lang, size) in langs {
                    let share = *size as f64 / *total_size as f64;
                    *ratings.entry(lang.clone()).or_insert(0.0) += *count as f64 * share;
                }
            } else if let Some((lang, _)) = langs.first() {
                // total_size is 0 (edge case): attribute everything to primary language.
                *ratings.entry(lang.clone()).or_insert(0.0) += *count as f64;
            }
            matched += 1;
        } else {
            unmatched += 1;
        }
    }

    eprintln!("  [{label}] {matched} repos matched, {unmatched} had no language data");

    let mut sorted: Vec<(String, f64)> = ratings.into_iter().collect();
    sorted.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    sorted
}

fn open(path: &PathBuf) -> Result<BufReader<File>> {
    File::open(path)
        .with_context(|| format!("cannot open {:?}", path))
        .map(BufReader::new)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_produce_statistics() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path();

        // Two repos: rust-lang/rust (Rust-heavy) and golang/go (Go-only).
        std::fs::write(
            dir.join("events-2024-01.csv"),
            r#"actor,repo,event_type,action,language,count
alice,rust-lang/rust,PullRequestEvent,opened,,3
bob,rust-lang/rust,PushEvent,,,5
alice,rust-lang/rust,IssuesEvent,opened,,2
alice,golang/go,PushEvent,,,1
carol,golang/go,PushEvent,,,4
carol,golang/go,WatchEvent,,,10
"#,
        )?;

        // rust-lang/rust: 90% Rust, 10% C.  golang/go: 100% Go.
        std::fs::write(
            dir.join("repo-languages-2024-01.jsonl"),
            r#"{"repo":"rust-lang/rust","total_size":1000,"languages":[{"language":"Rust","size":900},{"language":"C","size":100}],"fetched_at":"2026-01-15T10:30:00Z"}
{"repo":"golang/go","total_size":500,"languages":[{"language":"Go","size":500}],"fetched_at":"2026-01-15T10:30:00Z"}
"#,
        )?;

        run(Args {
            events: dir.join("events-2024-01.csv"),
            repo_languages: dir.join("repo-languages-2024-01.jsonl"),
            output_dir: dir.to_path_buf(),
            cap_single_actor_events: false,
        })?;

        // ── pr-count ─────────────────────────────────────────────────────────
        // Only rust-lang/rust had PRs (count=3). Rust gets 90%, C gets 10%.
        assert_eq!(
            std::fs::read_to_string(dir.join("language-ratings-2024-01-pr-count.jsonl"))?,
            r#"{"language":"Rust","rating":2.7,"percentage":90.0}
{"language":"C","rating":0.3,"percentage":10.0}
"#
        );

        // ── push-count ───────────────────────────────────────────────────────
        // rust-lang/rust: 5 pushes → Rust 4.5, C 0.5.  golang/go: alice 1 + carol 4 = 5 pushes → Go 5.0.
        // Total = 10.0 → Go 50.0%, Rust 45.0%, C 5.0%
        assert_eq!(
            std::fs::read_to_string(dir.join("language-ratings-2024-01-push-count.jsonl"))?,
            r#"{"language":"Go","rating":5.0,"percentage":50.0}
{"language":"Rust","rating":4.5,"percentage":45.0}
{"language":"C","rating":0.5,"percentage":5.0}
"#
        );

        // ── issue-count ──────────────────────────────────────────────────────
        // Only rust-lang/rust had issues (count=2). Rust 90%, C 10%.
        assert_eq!(
            std::fs::read_to_string(dir.join("language-ratings-2024-01-issue-count.jsonl"))?,
            r#"{"language":"Rust","rating":1.8,"percentage":90.0}
{"language":"C","rating":0.2,"percentage":10.0}
"#
        );

        // ── active-repos ─────────────────────────────────────────────────────
        // Both repos are active (each counts as 1).
        // rust-lang/rust → Rust 0.9, C 0.1.  golang/go → Go 1.0.
        // Total = 2.0 → Go 50.0%, Rust 45.0%, C 5.0%
        assert_eq!(
            std::fs::read_to_string(dir.join("language-ratings-2024-01-active-repos.jsonl"))?,
            r#"{"language":"Go","rating":1.0,"percentage":50.0}
{"language":"Rust","rating":0.9,"percentage":45.0}
{"language":"C","rating":0.1,"percentage":5.0}
"#
        );

        // ── star-count ───────────────────────────────────────────────────────
        // golang/go had 10 WatchEvents → Go 10.0.
        assert_eq!(
            std::fs::read_to_string(dir.join("language-ratings-2024-01-star-count.jsonl"))?,
            r#"{"language":"Go","rating":10.0,"percentage":100.0}
"#
        );

        Ok(())
    }
}
