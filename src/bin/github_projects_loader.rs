//! github-active-projects-loader
//!
//! NOTE: This tool is NOT used in the current pipeline.
//! The active approach is `github_archive_loader`, which downloads GH Archive
//! hourly files directly — no GCP credentials or BigQuery billing required.
//! This BigQuery-based loader is kept here in case it becomes useful again
//! in the future (e.g. for historical back-fills or cross-validation).
//!
//! Queries the GitHub Archive BigQuery dataset for the most active repositories
//! in a given month (measured by non-bot merged PullRequestEvents) and streams the
//! results as JSONL to stdout.
//!
//! stdout — JSONL payload (one JSON object per line); safe to pipe or redirect.
//! stderr — progress and error messages only; never mixed into the JSON output.
//!
//! Usage:
//!   # Built-in query for January 2026 (default month):
//!   github-active-projects-loader --project my-project > result.jsonl
//!
//!   # Different month:
//!   github-active-projects-loader --project my-project --month 202503 > result.jsonl
//!
//!   # Service-account key (omit to use Application Default Credentials):
//!   GOOGLE_APPLICATION_CREDENTIALS=/path/to/key.json
//!
//! Output format (stdout):
//!   {"repo":"owner/repo","count":"42"}
//!   {"repo":"owner/repo2","count":"17"}

use anyhow::{Context, Result};
use clap::Parser;
use futures::StreamExt;
use gcp_bigquery_client::{model::job_configuration_query::JobConfigurationQuery, Client};
use serde_json::Value;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::Instant;

/// Column names returned by build_query(), in select order.
const COLUMNS: &[&str] = &["repo", "count"];

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "github-active-projects-loader",
    about = "Query GitHub Archive via BigQuery and stream results as JSONL to stdout"
)]
struct Args {
    /// GCP project ID that will be billed for the query
    #[arg(long)]
    project: String,

    /// Rows to fetch per page (tune to balance memory vs. round-trips)
    #[arg(long, default_value_t = 10_000)]
    page_size: i32,

    /// Path to a service-account key JSON file.
    /// Overrides GOOGLE_APPLICATION_CREDENTIALS; omit to use Application Default Credentials.
    #[arg(long, env = "GOOGLE_APPLICATION_CREDENTIALS")]
    credentials: Option<PathBuf>,

    /// Archive month to query, in YYYYMM format (e.g. 202601 for January 2026).
    #[arg(long, default_value = "202601")]
    month: String,
}

// ── Built-in query ───────────────────────────────────────────────────────────

/// Returns the default active-projects SQL for the given archive month (YYYYMM).
/// Counts merged PullRequestEvents per repository, excluding actors that are
/// either identified as bots (URL contains "bot") OR are high-volume automated
/// accounts (>1000 events/month).
fn build_query(month: &str) -> String {
    format!(
        r#"SELECT repo.name, count(*) as count
FROM `githubarchive.month.{month}`
WHERE type = 'PullRequestEvent'
  AND JSON_VALUE(payload, '$.action') = 'merged'
  AND LOWER(actor.login) NOT LIKE "%bot%"
  AND actor.id NOT IN (
    SELECT actor.id
    FROM `githubarchive.month.{month}`
    GROUP BY actor.id
    HAVING COUNT(*) > 1000
  )
GROUP BY repo.name
ORDER BY count DESC"#
    )
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let sql = build_query(&args.month);
    let client = build_client(&args).await?;

    eprintln!(
        "Submitting query to BigQuery project '{}' (month: {}) …",
        args.project, args.month
    );
    let t0 = Instant::now();

    let query_config = JobConfigurationQuery {
        query: sql,
        use_legacy_sql: Some(false),
        ..Default::default()
    };

    let stdout = std::io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    let mut written = 0u64;

    let stream = client
        .job()
        .query_all(&args.project, query_config, Some(args.page_size));
    tokio::pin!(stream);

    while let Some(result) = stream.next().await {
        let rows = result.context("BigQuery page error")?;
        for row in rows {
            let cells = row.columns.as_deref().unwrap_or(&[]);
            let obj: serde_json::Map<String, Value> = COLUMNS
                .iter()
                .zip(cells.iter())
                .map(|(&col, cell)| (col.to_owned(), cell.value.clone().unwrap_or(Value::Null)))
                .collect();
            serde_json::to_writer(&mut writer, &Value::Object(obj))
                .context("failed to serialise row")?;
            writer.write_all(b"\n")?;
            written += 1;
        }
        eprintln!("  {} rows written …", written);
    }

    writer.flush()?;
    let elapsed = t0.elapsed().as_secs_f64();
    eprintln!("\nDone. {written} rows ({elapsed:.2}s total)");
    Ok(())
}

// ── Auth ─────────────────────────────────────────────────────────────────────

async fn build_client(args: &Args) -> Result<Client> {
    match &args.credentials {
        Some(path) => {
            let path_str = path
                .to_str()
                .context("credentials path is not valid UTF-8")?;
            Client::from_service_account_key_file(path_str)
                .await
                .context("failed to load service-account credentials")
        }
        None => Client::from_application_default_credentials()
            .await
            .context("failed to initialise Application Default Credentials"),
    }
}
