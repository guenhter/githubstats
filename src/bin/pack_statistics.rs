//! pack-statistics
//!
//! Reads all per-month language-ratings JSONL files for a given statistic type
//! and packs them into a single file, preserving every record as-is but adding
//! a "month" field.
//!
//! Input files (from `produce_statistics`):
//!   <input-dir>/language-ratings-YYYY-MM-<type>.jsonl
//!
//! Output file:
//!   <output-dir>/language-ratings-all-<type>.jsonl
//!
//! Each input record:
//!   {"language":"TypeScript","rating":322361.9,"percentage":16.2}
//!
//! Each output record:
//!   {"month":"2024-01","language":"TypeScript","rating":322361.9,"percentage":16.2}
//!
//! Records are written in chronological order (sorted by month), preserving the
//! within-month ordering (descending by rating) from the source files.
//!
//! Statistic types: pr-count | issue-count | push-count | developer-activity | active-repos
//!
//! Usage:
//!   pack_statistics --type pr-count
//!   pack_statistics --type active-repos --input-dir data/ --output-dir data/

use anyhow::{bail, Context, Result};
use clap::Parser;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "pack-statistics",
    about = "Pack all per-month language-ratings JSONL files into one file, adding a 'month' field.\n\
             Output: language-ratings-all-<type>.jsonl"
)]
struct Args {
    /// Statistic type to pack.
    /// One of: pr-count, issue-count, push-count, developer-activity, active-repos
    #[arg(long, value_name = "TYPE")]
    r#type: String,

    /// Directory containing the per-month language-ratings-YYYY-MM-<type>.jsonl files.
    #[arg(long, default_value = "data")]
    input_dir: PathBuf,

    /// Directory where the packed output file will be written.
    #[arg(long, default_value = "data")]
    output_dir: PathBuf,
}

// ── Main ─────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let args = Args::parse();

    validate_type(&args.r#type)?;

    // Collect all matching input files, sorted chronologically.
    let mut input_files = collect_input_files(&args.input_dir, &args.r#type)?;
    if input_files.is_empty() {
        bail!(
            "no language-ratings-*-{}.jsonl files found in {:?}",
            args.r#type,
            args.input_dir
        );
    }
    input_files.sort();

    eprintln!("Found {} monthly files for type '{}'", input_files.len(), args.r#type);

    // Open output file.
    std::fs::create_dir_all(&args.output_dir)
        .with_context(|| format!("cannot create output dir {:?}", args.output_dir))?;

    let out_path = args
        .output_dir
        .join(format!("language-ratings-all-{}.jsonl", args.r#type));
    eprintln!("Writing {:?} …", out_path);

    let file = File::create(&out_path)
        .with_context(|| format!("cannot create {:?}", out_path))?;
    let mut w = BufWriter::new(file);

    // Stream each monthly file, injecting the "month" field into every record.
    let mut total_records = 0usize;
    for path in &input_files {
        let month = month_from_path(path);
        let count = pack_file(path, &month, &mut w)
            .with_context(|| format!("failed to read {:?}", path))?;
        eprintln!("  {month}: {count} records");
        total_records += count;
    }

    eprintln!("Done. {total_records} records written.");
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn validate_type(t: &str) -> Result<()> {
    const VALID: &[&str] = &["pr-count", "issue-count", "push-count", "developer-activity", "active-repos"];
    if VALID.contains(&t) {
        Ok(())
    } else {
        bail!(
            "unknown type {:?}; valid values are: {}",
            t,
            VALID.join(", ")
        )
    }
}

/// Return all `language-ratings-YYYY-MM-<type>.jsonl` files in `dir`.
/// Excludes the packed output file (`language-ratings-all-…`).
fn collect_input_files(dir: &PathBuf, stat_type: &str) -> Result<Vec<PathBuf>> {
    let suffix = format!("-{stat_type}.jsonl");

    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read directory {:?}", dir))?;

    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.context("directory entry error")?;
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if name.starts_with("language-ratings-")
            && name.ends_with(&suffix)
            && !name.starts_with("language-ratings-all-")
        {
            files.push(entry.path());
        }
    }
    Ok(files)
}

/// Read one monthly ratings file and write every record to `w`, injecting
/// `"month": month` as the first field.  Returns the number of records written.
fn pack_file(path: &PathBuf, month: &str, w: &mut BufWriter<File>) -> Result<usize> {
    let reader = BufReader::new(
        File::open(path).with_context(|| format!("cannot open {:?}", path))?,
    );
    let mut count = 0;
    for (i, line) in reader.lines().enumerate() {
        let line = line.context("read error")?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Parse as a generic JSON object so we can inject the month field
        // without re-serialising the floating-point values through our types.
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(mut obj) => {
                if let Some(map) = obj.as_object_mut() {
                    // Insert "month" at the front by rebuilding the map in order.
                    let mut ordered = serde_json::Map::with_capacity(map.len() + 1);
                    ordered.insert("month".to_string(), serde_json::Value::String(month.to_string()));
                    ordered.extend(map.iter().map(|(k, v)| (k.clone(), v.clone())));
                    serde_json::to_writer(&mut *w, &serde_json::Value::Object(ordered))
                        .context("serialise")?;
                    w.write_all(b"\n")?;
                    count += 1;
                } else {
                    eprintln!("  [skip] {:?} line {}: not a JSON object", path, i + 1);
                }
            }
            Err(e) => eprintln!("  [skip] {:?} line {}: {e}", path, i + 1),
        }
    }
    Ok(count)
}

/// Extract "YYYY-MM" from a path like `.../language-ratings-2024-01-pr-count.jsonl`.
fn month_from_path(path: &PathBuf) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    // The 7-char YYYY-MM sits right after "language-ratings-".
    if let Some(rest) = name.strip_prefix("language-ratings-") {
        if rest.len() >= 7 {
            return rest[..7].to_string();
        }
    }
    name
}
