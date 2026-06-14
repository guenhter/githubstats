//! produce-statistics
//!
//! Joins an archive CSV file (output of `github_archive_loader`) with a
//! projects-languages JSONL file and computes a weighted language rating.
//!
//! The archive CSV is filtered to `PullRequestEvent` rows; their counts are
//! summed per repository to obtain a PR-activity score.  That score is then
//! distributed across languages proportionally to each language's byte share.
//!
//! Rating formula (per project P with N pull-request events):
//!   For each language L used by P at percentage pct:
//!     language_rating[L] += N × (pct / 100)
//!
//! Example: a repo with 10 PR events that is 70 % Python contributes 7.0 to Python.
//!
//! Input formats:
//!   --archive   CSV: repo,event_type,action,language,count
//!               (output of github_archive_loader)
//!   --languages JSONL: {"repo":"owner/repo","languages":[{"language":"Rust","percent":92.3},…]}
//!               (output of github_language_loader)
//!
//! Output (stdout) — JSONL sorted by rating descending:
//!   {"language":"Rust","rating":12345.6}
//!   {"language":"Python","rating":9876.5}
//!
//! All progress and diagnostic messages go to stderr.

use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "produce-statistics",
    about = "Compute weighted language ratings from an archive CSV and language breakdowns"
)]
struct Args {
    /// Archive CSV file produced by github_archive_loader
    /// Format: repo,event_type,action,language,count
    #[arg(long)]
    archive: PathBuf,

    /// JSONL file with per-project language breakdowns
    /// Format: {"repo":"owner/repo","languages":[{"language":"Rust","percent":92.3},…]}
    #[arg(long)]
    languages: PathBuf,
}

// ── Data types ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ProjectLanguages {
    repo: String,
    languages: Vec<LanguageEntry>,
}

#[derive(Deserialize)]
struct LanguageEntry {
    language: String,
    percent: f64,
}

// ── Main ─────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let args = Args::parse();

    eprintln!("Loading languages from {:?} …", args.languages);
    let lang_map = load_languages(&args.languages)?;
    eprintln!("  {} repos with language data", lang_map.len());

    eprintln!("Computing ratings from {:?} …", args.archive);
    let ratings = compute_ratings(&args.archive, &lang_map)?;

    let mut sorted: Vec<(String, f64)> = ratings.into_iter().collect();
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let stdout = std::io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    for (language, rating) in &sorted {
        let rating = (rating * 100.0).round() / 100.0;
        serde_json::to_writer(
            &mut writer,
            &json!({"language": language, "rating": rating}),
        )
        .context("serialise")?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;

    eprintln!("\nDone. {} languages written.", sorted.len());
    Ok(())
}

// ── I/O helpers ───────────────────────────────────────────────────────────────

/// Load the languages JSONL into a map keyed by repo slug.
fn load_languages(path: &PathBuf) -> Result<HashMap<String, Vec<(String, f64)>>> {
    let reader = open(path)?;
    let mut map: HashMap<String, Vec<(String, f64)>> = HashMap::new();
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
                    pl.languages
                        .into_iter()
                        .map(|e| (e.language, e.percent))
                        .collect(),
                );
            }
            Err(e) => eprintln!("  [skip] languages line {}: {e}", i + 1),
        }
    }
    Ok(map)
}

/// Read the archive CSV, sum `count` for `PullRequestEvent` rows per repo,
/// then accumulate weighted ratings per language.
///
/// CSV format (first row is header):
///   repo,event_type,action,language,count
fn compute_ratings(
    path: &PathBuf,
    lang_map: &HashMap<String, Vec<(String, f64)>>,
) -> Result<HashMap<String, f64>> {
    let reader = open(path)?;
    let mut pr_counts: HashMap<String, u64> = HashMap::new();
    let mut parse_errors = 0u64;

    for (i, line) in reader.lines().enumerate() {
        let line = line.context("read error")?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Skip header row
        if i == 0 && line.starts_with("repo,") {
            continue;
        }

        // Split CSV line into fields. Fields may be quoted (RFC 4180), but
        // repo and event_type never contain commas or quotes in practice, so
        // a simple split on ',' up to 5 fields is safe and avoids a csv crate dep.
        let fields: Vec<&str> = line.splitn(5, ',').collect();
        if fields.len() < 5 {
            eprintln!("  [skip] CSV line {}: expected 5 fields, got {}", i + 1, fields.len());
            parse_errors += 1;
            continue;
        }
        let repo = fields[0].trim_matches('"');
        let event_type = fields[1].trim_matches('"');
        let count_str = fields[4].trim_matches('"');

        // Only count PullRequestEvent rows.
        if event_type != "PullRequestEvent" {
            continue;
        }

        let count: u64 = match count_str.parse() {
            Ok(v) => v,
            Err(_) => {
                eprintln!("  [skip] non-numeric count on CSV line {}", i + 1);
                parse_errors += 1;
                continue;
            }
        };

        *pr_counts.entry(repo.to_string()).or_insert(0) += count;
    }

    eprintln!(
        "  {} repos with PullRequestEvent counts ({} parse errors)",
        pr_counts.len(),
        parse_errors
    );

    let mut ratings: HashMap<String, f64> = HashMap::new();
    let mut matched = 0u64;
    let mut unmatched = 0u64;

    for (repo, count) in &pr_counts {
        if let Some(langs) = lang_map.get(repo.as_str()) {
            for (lang, pct) in langs {
                *ratings.entry(lang.clone()).or_insert(0.0) += *count as f64 * (pct / 100.0);
            }
            matched += 1;
        } else {
            unmatched += 1;
        }
    }

    eprintln!("  {matched} repos matched, {unmatched} had no language data");
    Ok(ratings)
}

fn open(path: &PathBuf) -> Result<BufReader<File>> {
    File::open(path)
        .with_context(|| format!("cannot open {:?}", path))
        .map(BufReader::new)
}
