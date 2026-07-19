//! produce-statistics
//!
//! Joins an archive CSV file (output of `github_archive_loader` / `filter_archive`)
//! with a projects-languages JSONL file and produces multiple language-rating files
//! in the specified output directory.
//!
//! Output files (JSONL, sorted descending by rating):
//!   language-ratings-YYYY-MM-pr-count.jsonl
//!   language-ratings-YYYY-MM-issue-count.jsonl
//!   language-ratings-YYYY-MM-push-count.jsonl
//!   language-ratings-YYYY-MM-developer-activity.jsonl
//!   language-ratings-YYYY-MM-active-repos.jsonl
//!   language-ratings-YYYY-MM-star-count.jsonl
//!
//! With --primary-only the filenames gain a "-primary" suffix:
//!   language-ratings-YYYY-MM-pr-count-primary.jsonl  (etc.)
//!
//! Rating formula (all metric types):
//!
//!   Default (proportional):
//!     For each repo, distribute the event count across all its languages
//!     weighted by byte share.
//!     pr-count:           rating[L] += pr_count            × (size_L / total_size)
//!     issue-count:        rating[L] += issue_count         × (size_L / total_size)
//!     push-count:         rating[L] += push_count          × (size_L / total_size)
//!     developer-activity: rating[L] += distinct_contributors × (size_L / total_size)
//!                         (distinct actors across PullRequestEvent + PushEvent)
//!     active-repos:       rating[L] += 1                   × (size_L / total_size)
//!                         (once per repo that had any PushEvent or PullRequestEvent)
//!     star-count:         rating[L] += star_count           × (size_L / total_size)
//!
//!   --primary-only (experimental):
//!     All credit goes to the single dominant (largest-by-bytes) language;
//!     secondary languages are ignored.  Score multiplier is always 1, not the
//!     fractional byte share.
//!     pr-count:           rating[primary] += pr_count
//!     issue-count:        rating[primary] += issue_count
//!     push-count:         rating[primary] += push_count
//!     developer-activity: rating[primary] += distinct_contributors
//!                         (distinct actors across PullRequestEvent + PushEvent)
//!     active-repos:       rating[primary] += 1
//!     star-count:         rating[primary] += star_count
//!
//! Event types read from the archive CSV:
//!   PullRequestEvent → pr-count, developer-activity, and active-repos
//!   IssuesEvent      → issue-count
//!   PushEvent        → push-count, developer-activity, and active-repos
//!   WatchEvent       → star-count
//!
//! Input formats:
//!   --archive   CSV: actor,repo,event_type,action,language,count
//!   --languages JSONL: {"repo":"…","total_size":N,"languages":[{"language":"Rust","size":N},…]}
//!
//! The YEAR and MONTH for the output filename are inferred from the archive
//! filename, which must contain the pattern YYYYMM (e.g. archive-202401.csv or
//! archive-202401-filtered.csv).
//!
//! All progress and diagnostic messages go to stderr.

use anyhow::{Context, Result, bail};
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
    about = "Compute weighted language ratings from an archive CSV and language breakdowns.\n\
             Produces multiple JSONL files in --output-dir, one per statistic type."
)]
struct Args {
    /// Archive CSV file produced by github_archive_loader / filter_archive.
    /// Format: actor,repo,event_type,action,language,count
    /// The filename must contain YYYYMM (e.g. archive-202401-filtered.csv).
    #[arg(long)]
    archive: PathBuf,

    /// JSONL file with per-project language breakdowns.
    /// Format: {"repo":"owner/repo","total_size":158498874,"languages":[{"language":"Rust","size":143102371},…]}
    #[arg(long)]
    languages: PathBuf,

    /// Directory where the output JSONL files will be written.
    /// Files are named: language-ratings-YYYY-MM-<type>.jsonl
    #[arg(long)]
    output_dir: PathBuf,

    /// Experimental: attribute all of a repo's score to its primary (largest)
    /// language only, with a weight of 1 instead of the fractional byte share.
    /// Secondary languages are ignored entirely.
    /// Output filenames gain a "-primary" suffix.
    #[arg(long, default_value_t = false)]
    primary_only: bool,
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

/// All per-repo activity counts collected from the archive CSV in a single pass.
struct RepoCounts {
    /// Total PullRequestEvent count per repo.
    pr_counts: HashMap<String, u64>,
    /// Distinct actors that generated a PullRequestEvent or PushEvent, per repo.
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
    let args = Args::parse();

    // Infer YYYY-MM from the archive filename.
    let year_month = infer_year_month(&args.archive)?;
    eprintln!("Inferred period: {year_month}");

    // Create output directory if it doesn't exist.
    std::fs::create_dir_all(&args.output_dir)
        .with_context(|| format!("cannot create output dir {:?}", args.output_dir))?;

    eprintln!("Loading languages from {:?} …", args.languages);
    let lang_map = load_languages(&args.languages)?;
    eprintln!("  {} repos with language data", lang_map.len());

    eprintln!("Reading activity from {:?} …", args.archive);
    let counts = collect_counts(&args.archive)?;
    eprintln!(
        "  {} repos with PR activity, {} with issue activity, {} with push activity, {} active repos, {} with star activity",
        counts.pr_counts.len(),
        counts.issue_counts.len(),
        counts.push_counts.len(),
        counts.active_repos.len(),
        counts.star_counts.len(),
    );

    // ── pr-count ─────────────────────────────────────────────────────────────
    let out = output_path(&args.output_dir, &year_month, "pr-count", args.primary_only);
    eprintln!("Writing {out:?} …");
    {
        let mut w = open_writer(&out)?;
        let ratings = compute_ratings(&counts.pr_counts, &lang_map, "PR", args.primary_only);
        write_ratings(&mut w, &ratings)?;
    }

    // ── issue-count ──────────────────────────────────────────────────────────
    let out = output_path(
        &args.output_dir,
        &year_month,
        "issue-count",
        args.primary_only,
    );
    eprintln!("Writing {out:?} …");
    {
        let mut w = open_writer(&out)?;
        let ratings = compute_ratings(&counts.issue_counts, &lang_map, "issue", args.primary_only);
        write_ratings(&mut w, &ratings)?;
    }

    // ── push-count ───────────────────────────────────────────────────────────
    let out = output_path(
        &args.output_dir,
        &year_month,
        "push-count",
        args.primary_only,
    );
    eprintln!("Writing {out:?} …");
    {
        let mut w = open_writer(&out)?;
        let ratings = compute_ratings(&counts.push_counts, &lang_map, "push", args.primary_only);
        write_ratings(&mut w, &ratings)?;
    }

    // ── developer-activity ───────────────────────────────────────────────────
    let out = output_path(
        &args.output_dir,
        &year_month,
        "developer-activity",
        args.primary_only,
    );
    eprintln!("Writing {out:?} …");
    {
        let mut w = open_writer(&out)?;
        // Convert contributor counts to u64 map so we can reuse compute_ratings.
        let dev_counts: HashMap<String, u64> = counts
            .dev_actors
            .iter()
            .map(|(repo, n)| (repo.clone(), *n as u64))
            .collect();
        let ratings = compute_ratings(
            &dev_counts,
            &lang_map,
            "developer-activity",
            args.primary_only,
        );
        write_ratings(&mut w, &ratings)?;
    }

    // ── active-repos ─────────────────────────────────────────────────────────
    let out = output_path(
        &args.output_dir,
        &year_month,
        "active-repos",
        args.primary_only,
    );
    eprintln!("Writing {out:?} …");
    {
        let mut w = open_writer(&out)?;
        let ratings = compute_ratings(
            &counts.active_repos,
            &lang_map,
            "active-repos",
            args.primary_only,
        );
        write_ratings(&mut w, &ratings)?;
    }

    // ── star-count ───────────────────────────────────────────────────────────
    let out = output_path(
        &args.output_dir,
        &year_month,
        "star-count",
        args.primary_only,
    );
    eprintln!("Writing {out:?} …");
    {
        let mut w = open_writer(&out)?;
        let ratings = compute_ratings(
            &counts.star_counts,
            &lang_map,
            "star-count",
            args.primary_only,
        );
        write_ratings(&mut w, &ratings)?;
    }

    eprintln!("Done.");
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract YYYY-MM from an archive filename that contains a YYYYMM digit sequence.
/// Accepts filenames like archive-202401.csv or archive-202401-filtered.csv.
fn infer_year_month(path: &PathBuf) -> Result<String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("cannot read filename from {:?}", path))?;

    // Find the first run of 6 consecutive ASCII digits.
    let chars: Vec<char> = name.chars().collect();
    for i in 0..chars.len().saturating_sub(5) {
        if chars[i..i + 6].iter().all(|c| c.is_ascii_digit()) {
            let year: String = chars[i..i + 4].iter().collect();
            let month: String = chars[i + 4..i + 6].iter().collect();
            return Ok(format!("{year}-{month}"));
        }
    }

    bail!(
        "cannot infer YYYY-MM from archive filename {:?}; \
         filename must contain a YYYYMM sequence (e.g. archive-202401-filtered.csv)",
        name
    );
}

/// Build the output file path for a given type.
/// With `primary_only` the filename gains a "-primary" suffix before ".jsonl".
fn output_path(dir: &Path, year_month: &str, kind: &str, primary_only: bool) -> PathBuf {
    if primary_only {
        dir.join(format!(
            "language-ratings-{year_month}-{kind}-primary.jsonl"
        ))
    } else {
        dir.join(format!("language-ratings-{year_month}-{kind}.jsonl"))
    }
}

/// Open a file for writing, wrapped in a BufWriter.
fn open_writer(path: &PathBuf) -> Result<BufWriter<File>> {
    File::create(path)
        .with_context(|| format!("cannot create {:?}", path))
        .map(BufWriter::new)
}

/// Write a sorted-descending list of (language, rating) pairs as JSONL.
/// Each record includes the rating and its percentage share of the total.
fn write_ratings(w: &mut BufWriter<File>, ratings: &[(String, f64)]) -> Result<()> {
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

/// Read the archive CSV in a single pass and accumulate counts for all
/// relevant event types.
///
/// CSV format (first row is header):
///   actor,repo,event_type,action,language,count
fn collect_counts(path: &PathBuf) -> Result<RepoCounts> {
    let reader = open(path)?;

    let mut pr_counts: HashMap<String, u64> = HashMap::new();
    let mut dev_actor_sets: HashMap<String, HashSet<String>> = HashMap::new();
    let mut issue_counts: HashMap<String, u64> = HashMap::new();
    let mut push_counts: HashMap<String, u64> = HashMap::new();
    let mut active_repo_set: HashSet<String> = HashSet::new();
    let mut star_counts: HashMap<String, u64> = HashMap::new();
    let mut parse_errors = 0u64;

    for (i, line) in reader.lines().enumerate() {
        let line = line.context("read error")?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Skip header row.
        if i == 0 && line.starts_with("actor,") {
            continue;
        }

        let fields: Vec<&str> = line.splitn(6, ',').collect();
        if fields.len() < 6 {
            eprintln!(
                "  [skip] CSV line {}: expected 6 fields, got {}",
                i + 1,
                fields.len()
            );
            parse_errors += 1;
            continue;
        }
        let actor = fields[0].trim_matches('"');
        let repo = fields[1].trim_matches('"');
        let event_type = fields[2].trim_matches('"');
        let count_str = fields[5].trim_matches('"');

        let count: u64 = match count_str.parse() {
            Ok(v) => v,
            Err(_) => {
                eprintln!("  [skip] non-numeric count on CSV line {}", i + 1);
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

/// Compute language ratings from a map of per-repo event counts.
///
/// Default (proportional): distribute each repo's count across all its
/// languages weighted by byte share.
///
/// Primary-only: attribute the full count to the single dominant language;
/// secondary languages are ignored.
fn compute_ratings(
    event_counts: &HashMap<String, u64>,
    lang_map: &LangMap,
    label: &str,
    primary_only: bool,
) -> Vec<(String, f64)> {
    let mut ratings: HashMap<String, f64> = HashMap::new();
    let mut matched = 0u64;
    let mut unmatched = 0u64;

    for (repo, count) in event_counts {
        if let Some((total_size, langs)) = lang_map.get(repo.as_str()) {
            if primary_only {
                // All credit to the primary (first = largest) language, weight = 1.
                if let Some((lang, _)) = langs.first() {
                    *ratings.entry(lang.clone()).or_insert(0.0) += *count as f64;
                }
            } else if *total_size > 0 {
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
