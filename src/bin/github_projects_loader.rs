//! github-projects-loader
//!
//! NOTE: This tool is NOT used in the current pipeline.
//! The active approach is `github_archive_loader`, which downloads GH Archive
//! hourly files directly — no GCP credentials or BigQuery billing required.
//! This BigQuery-based loader is kept here in case it becomes useful again
//! in the future (e.g. for historical back-fills or cross-validation).
//!
//! Queries the GitHub Archive BigQuery dataset for a given month and writes
//! aggregated event counts to a CSV file, matching the output format and
//! filter rules of `github_archive_loader`.
//!
//! stdout — progress and error messages only.
//!
//! Usage:
//!   github-projects-loader --project my-project --year 2026 --month 1 --output events.csv
//!
//!   # Service-account key (omit to use Application Default Credentials):
//!   GOOGLE_APPLICATION_CREDENTIALS=/path/to/key.json
//!
//! Output format (CSV):
//!   repo,event_type,action,language,count
//!   owner/name,PullRequestEvent,closed,Rust,42

use anyhow::{Context, Result};
use clap::Parser;
use futures::StreamExt;
use gcp_bigquery_client::{model::job_configuration_query::JobConfigurationQuery, Client};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::Instant;

/// Column names returned by build_query(), in select order.
const COLUMNS: &[&str] = &["repo", "event_type", "action", "language", "count"];

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "github-projects-loader",
    about = "Query GitHub Archive via BigQuery and write aggregated event counts as CSV"
)]
struct Args {
    /// GCP project ID that will be billed for the query
    #[arg(long)]
    project: String,

    /// Year to query (e.g. 2026)
    #[arg(long, value_parser = clap::value_parser!(i32).range(2011..))]
    year: i32,

    /// Month to query (1–12)
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=12))]
    month: u32,

    /// Rows to fetch per page (tune to balance memory vs. round-trips)
    #[arg(long, default_value_t = 10_000)]
    page_size: i32,

    /// Path to a service-account key JSON file.
    /// Overrides GOOGLE_APPLICATION_CREDENTIALS; omit to use Application Default Credentials.
    #[arg(long, env = "GOOGLE_APPLICATION_CREDENTIALS")]
    credentials: Option<PathBuf>,

    /// Path for the output CSV file
    #[arg(long)]
    output: PathBuf,
}

// ── Built-in query ───────────────────────────────────────────────────────────

/// Returns SQL that replicates the filtering and aggregation of
/// `github_archive_loader`:
///   - excludes bot actors (login contains "bot")
///   - excludes high-volume actors (> 1 000 events/month)
///   - groups by (repo, event_type, action, language)
///   - language is extracted from the payload JSON (same paths as the
///     archive loader: PullRequestEvent → pull_request.head.repo.language,
///     IssuesEvent → issue.repository.language, others → repository.language)
fn build_query(year: i32, month: u32) -> String {
    let ym = format!("{year}{month:02}");
    format!(
        r#"SELECT
  repo.name AS repo,
  type      AS event_type,
  COALESCE(JSON_VALUE(payload, '$.action'), '') AS action,
  COALESCE(
    JSON_VALUE(payload, '$.pull_request.head.repo.language'),
    JSON_VALUE(payload, '$.issue.repository.language'),
    JSON_VALUE(payload, '$.repository.language'),
    ''
  ) AS language,
  COUNT(*) AS count
FROM `githubarchive.month.{ym}`
WHERE LOWER(actor.login) NOT LIKE '%bot%'
  AND actor.id NOT IN (
    SELECT actor.id
    FROM `githubarchive.month.{ym}`
    GROUP BY actor.id
    HAVING COUNT(*) > 1000
  )
  AND repo.name LIKE '%/%'
GROUP BY repo, event_type, action, language
ORDER BY count DESC"#
    )
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let sql = build_query(args.year, args.month);
    let client = build_client(&args).await?;

    eprintln!(
        "Submitting query to BigQuery project '{}' ({}-{:02}) …",
        args.project, args.year, args.month
    );
    let t0 = Instant::now();

    let query_config = JobConfigurationQuery {
        query: sql,
        use_legacy_sql: Some(false),
        ..Default::default()
    };

    let file = File::create(&args.output)
        .with_context(|| format!("create {:?}", args.output))?;
    let mut writer = BufWriter::new(file);
    writer.write_all(b"repo,event_type,action,language,count\n")
        .context("write CSV header")?;

    let mut written = 0usize;

    let stream = client
        .job()
        .query_all(&args.project, query_config, Some(args.page_size));
    tokio::pin!(stream);

    while let Some(result) = stream.next().await {
        let rows = result.context("BigQuery page error")?;
        for row in rows {
            let cells = row.columns.as_deref().unwrap_or(&[]);
            let vals: Vec<&str> = COLUMNS
                .iter()
                .zip(cells.iter())
                .map(|(_, cell)| {
                    cell.value
                        .as_ref()
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                })
                .collect();

            if vals.len() == COLUMNS.len() {
                let repo     = csv_field(vals[0]);
                let etype    = csv_field(vals[1]);
                let action   = csv_field(vals[2]);
                let language = csv_field(vals[3]);
                let count    = vals[4];
                writeln!(writer, "{repo},{etype},{action},{language},{count}")
                    .context("write CSV row")?;
                written += 1;
            }
        }
        eprintln!("  {} rows written …", written);
    }

    writer.flush()?;
    eprintln!(
        "  [writer] {} rows written to {:?}",
        written, args.output
    );
    eprintln!("Done in {:.1}s", t0.elapsed().as_secs_f64());
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

// ── CSV helpers ───────────────────────────────────────────────────────────────

/// Quotes a CSV field if it contains a comma, double-quote, or newline.
fn csv_field(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains([',', '"', '\n', '\r']) {
        std::borrow::Cow::Owned(format!("\"{}\"", s.replace('"', "\"\"")))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}
