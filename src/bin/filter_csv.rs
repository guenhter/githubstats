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
//!   filter_high_volume_actors — drops rows belonging to actors whose total
//!                               event count across the whole file exceeds a
//!                               threshold (default: 1 000).
//!
//! Usage:
//!   filter_csv --input archive-202605.csv
//!   filter_csv --input archive-202605.csv --actor-event-limit 500
//!
//! Output: same directory as input, filename with `-filtered` inserted before
//! the extension, e.g. `archive-202605-filtered.csv`.

use anyhow::{Context, Result};
use clap::Parser;
use std::collections::HashMap;
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
    actor:      String,
    repo:       String,
    event_type: String,
    action:     String,
    language:   String,
    count:      u64,
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let args = Args::parse();

    let rows = read_csv(&args.input)?;
    eprintln!("  [read]   {} rows", rows.len());

    let rows = filter_bots(rows);
    let rows = filter_high_volume_actors(rows, args.actor_event_limit);

    let output = output_path(&args.input)?;
    write_csv(rows, &output)?;

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
    eprintln!(
        "  [filter_bots]                {} removed, {} remaining",
        before - rows.len(),
        rows.len(),
    );
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

    eprintln!(
        "  [filter_high_volume_actors]  {} removed, {} remaining  (limit={})",
        before - rows.len(),
        rows.len(),
        limit,
    );
    rows
}

// ── I/O ───────────────────────────────────────────────────────────────────────

/// Reads the CSV into a `Vec<Row>`, skipping the header and any malformed lines.
fn read_csv(path: &Path) -> Result<Vec<Row>> {
    let file = File::open(path).with_context(|| format!("open {path:?}"))?;
    let reader = BufReader::with_capacity(1 << 20, file);
    let mut lines = reader.lines();

    // Consume and validate header
    let header = lines
        .next()
        .with_context(|| "file is empty")??;
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
            Err(_) => { bad += 1; continue; }
        };
        rows.push(Row {
            actor:      fields[0].clone(),
            repo:       fields[1].clone(),
            event_type: fields[2].clone(),
            action:     fields[3].clone(),
            language:   fields[4].clone(),
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
        let actor      = csv_field(&r.actor);
        let repo       = csv_field(&r.repo);
        let event_type = csv_field(&r.event_type);
        let action     = csv_field(&r.action);
        let language   = csv_field(&r.language);
        writeln!(w, "{actor},{repo},{event_type},{action},{language},{}", r.count)
            .context("write row")?;
    }

    w.flush().context("flush")?;
    eprintln!("  [write]  {} rows written to {path:?}", rows.len());
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
