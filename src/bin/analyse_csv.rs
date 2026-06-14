//! analyse_csv
//!
//! Streams through a githubstats CSV file (repo,event_type,action,language,count)
//! without loading it entirely into RAM and reports statistics useful for
//! sanity-checking / deciding what else to filter.
//!
//! Usage:
//!   cargo run --bin analyse_csv -- --input archive-202605.csv

use anyhow::{Context, Result};
use clap::Parser;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "analyse_csv", about = "Sanity-analyse a githubstats CSV file")]
struct Args {
    /// Path to the CSV file
    #[arg(long)]
    input: PathBuf,

    /// How many top entries to print per category
    #[arg(long, default_value_t = 30)]
    top: usize,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let file = File::open(&args.input)
        .with_context(|| format!("open {:?}", args.input))?;
    let reader = BufReader::with_capacity(1 << 20, file); // 1 MiB read buffer

    // ── Accumulators ─────────────────────────────────────────────────────────

    let mut total_rows: u64 = 0;
    let mut total_count_sum: u64 = 0;

    // Structural / field-level issues
    let mut bad_rows: u64 = 0;           // wrong number of fields
    let mut empty_repo: u64 = 0;
    let mut empty_event_type: u64 = 0;
    let mut zero_count: u64 = 0;
    let mut non_numeric_count: u64 = 0;

    // Language coverage
    let mut rows_with_language: u64 = 0;
    let mut count_with_language: u64 = 0;

    // Distributions (count-weighted)
    let mut event_type_counts: HashMap<String, u64> = HashMap::new();
    let mut language_counts: HashMap<String, u64> = HashMap::new();
    let mut action_counts: HashMap<String, u64> = HashMap::new();

    // Repo-level aggregation (row count per repo, for spotting spammy repos)
    let mut repo_row_count: HashMap<String, u64> = HashMap::new();
    let mut repo_event_count: HashMap<String, u64> = HashMap::new(); // sum of `count` per repo

    // Suspicious repo patterns
    let mut repo_no_owner: u64 = 0;          // no slash → already counted above, kept separate for clarity
    let mut repo_dot_git_suffix: u64 = 0;    // ends with .git
    let mut repo_has_space: u64 = 0;
    let mut repo_very_long: u64 = 0;         // owner or name part > 100 chars (github max is 100)
    let mut repo_non_ascii: u64 = 0;

    // Language anomalies
    let mut lang_null_string: u64 = 0;       // literal "null" or "NULL"
    let mut lang_very_long: u64 = 0;         // > 64 chars (no real language is that long)

    // Count distribution buckets (how many rows have count in range?)
    let mut count_1: u64 = 0;
    let mut count_2_10: u64 = 0;
    let mut count_11_100: u64 = 0;
    let mut count_101_1000: u64 = 0;
    let mut count_over_1000: u64 = 0;

    // Extreme-count rows — keep top-N by count (repo, event_type, count)
    // We use a simple vec + sort at the end (capped at top*10 during streaming).
    let mut extreme_rows: Vec<(u64, String, String, String)> = Vec::new(); // (count, repo, event_type, language)
    let extreme_cap = args.top * 20;

    // ── Stream ────────────────────────────────────────────────────────────────

    let mut lines = reader.lines();

    // Skip header
    let header = lines.next().context("file is empty")??;
    if !header.starts_with("repo,") {
        eprintln!("WARN: unexpected header: {header}");
    }

    for line_result in lines {
        let line = line_result.context("I/O error reading line")?;
        if line.is_empty() {
            continue;
        }
        total_rows += 1;

        // ── Parse fields (simple split; our CSV escapes commas in quotes) ────
        let fields = split_csv_line(&line);
        if fields.len() != 5 {
            bad_rows += 1;
            continue;
        }

        let repo       = fields[0].as_str();
        let event_type = fields[1].as_str();
        let action     = fields[2].as_str();
        let language   = fields[3].as_str();
        let count_str  = fields[4].as_str();

        // ── Count field ───────────────────────────────────────────────────────
        let count: u64 = match count_str.trim().parse::<u64>() {
            Ok(n) => n,
            Err(_) => {
                non_numeric_count += 1;
                continue;
            }
        };

        if count == 0 {
            zero_count += 1;
        }

        total_count_sum += count;

        // Count distribution
        match count {
            1          => count_1 += 1,
            2..=10     => count_2_10 += 1,
            11..=100   => count_11_100 += 1,
            101..=1000 => count_101_1000 += 1,
            _          => count_over_1000 += 1,
        }

        // Extreme rows reservoir
        if count > 500 {
            extreme_rows.push((count, repo.to_string(), event_type.to_string(), language.to_string()));
            if extreme_rows.len() > extreme_cap {
                extreme_rows.sort_unstable_by_key(|b| std::cmp::Reverse(b.0));
                extreme_rows.truncate(extreme_cap / 2);
            }
        }

        // ── Repo checks ───────────────────────────────────────────────────────
        if repo.is_empty() {
            empty_repo += 1;
        } else if !repo.contains('/') {
            repo_no_owner += 1;
        } else {
            let parts: Vec<&str> = repo.splitn(2, '/').collect();
            let owner = parts[0];
            let name  = parts[1];
            if owner.len() > 100 || name.len() > 100 {
                repo_very_long += 1;
            }
            if !repo.is_ascii() {
                repo_non_ascii += 1;
            }
            if repo.contains(' ') {
                repo_has_space += 1;
            }
            if name.ends_with(".git") {
                repo_dot_git_suffix += 1;
            }
        }

        // Per-repo aggregation (only valid repos)
        if !repo.is_empty() && repo.contains('/') {
            *repo_row_count.entry(repo.to_string()).or_insert(0) += 1;
            *repo_event_count.entry(repo.to_string()).or_insert(0) += count;
        }

        // ── Event type ────────────────────────────────────────────────────────
        if event_type.is_empty() {
            empty_event_type += 1;
        }
        *event_type_counts.entry(event_type.to_string()).or_insert(0) += count;

        // ── Action ────────────────────────────────────────────────────────────
        let action_key = if action.is_empty() { "(empty)" } else { action };
        *action_counts.entry(action_key.to_string()).or_insert(0) += count;

        // ── Language ──────────────────────────────────────────────────────────
        if language.is_empty() {
            // no language — fine, already handled by absence
        } else {
            let lang_lc = language.to_ascii_lowercase();
            if lang_lc == "null" {
                lang_null_string += 1;
            }
            if language.len() > 64 {
                lang_very_long += 1;
            }
            rows_with_language += 1;
            count_with_language += count;
            *language_counts.entry(language.to_string()).or_insert(0) += count;
        }
    }

    // ── Sort helpers ──────────────────────────────────────────────────────────

    let top_n = args.top;

    let mut ev_sorted: Vec<(&String, &u64)> = event_type_counts.iter().collect();
    ev_sorted.sort_unstable_by(|a, b| b.1.cmp(a.1));

    let mut lang_sorted: Vec<(&String, &u64)> = language_counts.iter().collect();
    lang_sorted.sort_unstable_by(|a, b| b.1.cmp(a.1));

    let mut action_sorted: Vec<(&String, &u64)> = action_counts.iter().collect();
    action_sorted.sort_unstable_by(|a, b| b.1.cmp(a.1));

    // Top repos by row count (many rows = many (event_type,action) combos = spammy?)
    let mut repo_rows_sorted: Vec<(&String, &u64)> = repo_row_count.iter().collect();
    repo_rows_sorted.sort_unstable_by(|a, b| b.1.cmp(a.1));

    // Top repos by total event count
    let mut repo_events_sorted: Vec<(&String, &u64)> = repo_event_count.iter().collect();
    repo_events_sorted.sort_unstable_by(|a, b| b.1.cmp(a.1));

    extreme_rows.sort_unstable_by_key(|b| std::cmp::Reverse(b.0));
    extreme_rows.truncate(top_n);

    // ── Print report ──────────────────────────────────────────────────────────

    println!("═══════════════════════════════════════════════════════════════");
    println!(" CSV SANITY REPORT: {:?}", args.input.file_name().unwrap_or_default());
    println!("═══════════════════════════════════════════════════════════════\n");

    println!("── Overview ────────────────────────────────────────────────────");
    println!("  Total data rows        : {:>14}", fmt_u64(total_rows));
    println!("  Sum of count column    : {:>14}", fmt_u64(total_count_sum));
    println!("  Unique repos           : {:>14}", fmt_u64(repo_row_count.len() as u64));
    println!("  Unique event types     : {:>14}", fmt_u64(event_type_counts.len() as u64));
    println!("  Unique languages       : {:>14}", fmt_u64(language_counts.len() as u64));
    println!();

    println!("── Structural Issues ────────────────────────────────────────────");
    println!("  Rows with wrong field count : {:>10}", fmt_u64(bad_rows));
    println!("  Rows with non-numeric count : {:>10}", fmt_u64(non_numeric_count));
    println!("  Rows with count = 0         : {:>10}", fmt_u64(zero_count));
    println!("  Rows with empty repo        : {:>10}", fmt_u64(empty_repo));
    println!("  Rows with empty event_type  : {:>10}", fmt_u64(empty_event_type));
    println!();

    println!("── Repo Anomalies ───────────────────────────────────────────────");
    println!("  No slash (no owner)         : {:>10}", fmt_u64(repo_no_owner));
    println!("  Has space in name           : {:>10}", fmt_u64(repo_has_space));
    println!("  Non-ASCII characters        : {:>10}", fmt_u64(repo_non_ascii));
    println!("  Name part ends with .git    : {:>10}", fmt_u64(repo_dot_git_suffix));
    println!("  Owner or name > 100 chars   : {:>10}", fmt_u64(repo_very_long));
    println!();

    println!("── Language Coverage ────────────────────────────────────────────");
    let lang_row_pct = 100.0 * rows_with_language as f64 / total_rows.max(1) as f64;
    let lang_cnt_pct = 100.0 * count_with_language as f64 / total_count_sum.max(1) as f64;
    println!("  Rows with non-empty language: {:>10}  ({:.1}% of rows)", fmt_u64(rows_with_language), lang_row_pct);
    println!("  Events with language        : {:>10}  ({:.1}% of event-count)", fmt_u64(count_with_language), lang_cnt_pct);
    println!("  Rows where language='null'  : {:>10}", fmt_u64(lang_null_string));
    println!("  Rows where language > 64 ch : {:>10}", fmt_u64(lang_very_long));
    println!();

    println!("── Count Distribution (rows) ────────────────────────────────────");
    println!("  count = 1        : {:>10}  ({:.1}%)", fmt_u64(count_1), pct(count_1, total_rows));
    println!("  count 2–10       : {:>10}  ({:.1}%)", fmt_u64(count_2_10), pct(count_2_10, total_rows));
    println!("  count 11–100     : {:>10}  ({:.1}%)", fmt_u64(count_11_100), pct(count_11_100, total_rows));
    println!("  count 101–1000   : {:>10}  ({:.1}%)", fmt_u64(count_101_1000), pct(count_101_1000, total_rows));
    println!("  count > 1000     : {:>10}  ({:.1}%)", fmt_u64(count_over_1000), pct(count_over_1000, total_rows));
    println!();

    println!("── Top {} Event Types (by sum of count) ─────────────────────────", top_n);
    for (i, (k, v)) in ev_sorted.iter().take(top_n).enumerate() {
        println!("  {:>3}. {:>12}  {}", i + 1, fmt_u64(**v), k);
    }
    println!();

    println!("── Top {} Languages (by sum of count) ───────────────────────────", top_n);
    for (i, (k, v)) in lang_sorted.iter().take(top_n).enumerate() {
        println!("  {:>3}. {:>12}  {}", i + 1, fmt_u64(**v), k);
    }
    if lang_sorted.len() > top_n {
        println!("  … {} more languages", lang_sorted.len() - top_n);
    }
    println!();

    println!("── Top {} Actions (by sum of count) ─────────────────────────────", top_n);
    for (i, (k, v)) in action_sorted.iter().take(top_n).enumerate() {
        println!("  {:>3}. {:>12}  {}", i + 1, fmt_u64(**v), k);
    }
    println!();

    println!("── Top {} Repos by Row Count (many rows = many event combos) ────", top_n);
    for (i, (k, v)) in repo_rows_sorted.iter().take(top_n).enumerate() {
        let ev_count = repo_event_count.get(*k).copied().unwrap_or(0);
        println!("  {:>3}. {:>6} rows  {:>8} events  {}", i + 1, v, fmt_u64(ev_count), k);
    }
    println!();

    println!("── Top {} Repos by Total Event Count ────────────────────────────", top_n);
    for (i, (k, v)) in repo_events_sorted.iter().take(top_n).enumerate() {
        let rows = repo_row_count.get(*k).copied().unwrap_or(0);
        println!("  {:>3}. {:>12} events  {:>5} rows  {}", i + 1, fmt_u64(**v), rows, k);
    }
    println!();

    println!("── Top {} Extreme Rows (count > 500) ────────────────────────────", top_n);
    if extreme_rows.is_empty() {
        println!("  (none)");
    }
    for (i, (count, repo, etype, lang)) in extreme_rows.iter().enumerate() {
        println!("  {:>3}. count={:>8}  {:40}  {:25}  {}", i + 1, fmt_u64(*count), repo, etype, lang);
    }
    println!();

    println!("═══════════════════════════════════════════════════════════════");
    println!(" END OF REPORT");
    println!("═══════════════════════════════════════════════════════════════");

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn fmt_u64(n: u64) -> String {
    // Insert thousands separators
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push('_');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

fn pct(part: u64, total: u64) -> f64 {
    if total == 0 { 0.0 } else { 100.0 * part as f64 / total as f64 }
}

/// Minimal CSV line splitter that handles RFC 4180 quoted fields.
/// Sufficient for our format: repo,event_type,action,language,count
fn split_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::with_capacity(5);
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes => {
                // Escaped quote ("") or end of quoted field
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            }
            '"' => {
                in_quotes = true;
            }
            ',' if !in_quotes => {
                fields.push(field.clone());
                field.clear();
            }
            other => {
                field.push(other);
            }
        }
    }
    fields.push(field);
    fields
}
