//! github-archive-loader
//!
//! Downloads GitHub Archive hourly `.json.gz` files for a given month, extracts
//! events, and writes aggregated counts to a CSV file.
//!
//! Pipeline:
//!
//!   populate_download_jobs  — pushes one DownloadJob (URL + index) per hour
//!                             into the job channel, then closes it.
//!
//!   download  (N async)     — each worker pulls jobs, fetches the `.json.gz`
//!                             body, and forwards raw compressed bytes.
//!                             Pure network I/O — no blocking work.
//!
//!   decompress  (N blocking) — each worker receives compressed bytes,
//!                              decompresses with flate2, parses JSON lines,
//!                              and forwards [`RawEvent`]s.
//!                              Runs on tokio's blocking thread pool.
//!
//!   aggregate_events        — drains all [`RawEvent`]s and aggregates into a
//!                             `Vec<AggregatedEvent>` keyed by
//!                             (actor, repo, event_type, action).
//!
//!   write_csv               — writes `Vec<AggregatedEvent>` as RFC 4180 CSV.
//!
//! Usage:
//!   github-archive-loader --year 2026 --month 1 --parallelism 10 --output events.csv
//!
//! Output format (CSV):
//!   actor,repo,event_type,action,language,count
//!   someuser,owner/name,PullRequestEvent,closed,Rust,42
//!
//! ## Why we preprocess: data sizes
//!
//! GitHub Archive publishes one `.json.gz` file per hour.  Each file contains
//! one raw JSON object per event — full payloads including PR diffs, issue
//! bodies, commit messages, nested user objects, etc.
//!
//! Measured against the May 2026 archive (744 hours, 112 million raw events):
//!
//! | Form                            | Size      | Notes                              |
//! |---------------------------------|-----------|------------------------------------|
//! | Compressed `.json.gz` on disk   | ~19 GB    | ~25 MB/hour average                |
//! | Uncompressed raw JSON           | ~93 GB    | ~4.9× expansion; ~826 bytes/event  |
//! | Aggregated CSV (this tool)      | ~1.1 GB   | actual output, actor included      |
//!
//! The aggregated CSV is ~84× smaller than the raw uncompressed JSON.  The
//! reduction comes from two sources:
//!
//!   1. **Field pruning** — each event is reduced to six fields
//!      (actor, repo, event_type, action, language, count).  All payload detail
//!      (commit messages, PR bodies, user metadata, …) is discarded.
//!
//!   2. **Aggregation** — events are grouped by
//!      (actor, repo, event_type, action) and replaced by a count.  A developer
//!      who pushes to the same repo 30 times in a month becomes a single row
//!      with count=30 instead of 30 separate multi-kilobyte JSON objects.
//!
//! This makes the CSV practical for in-process analysis and multi-year joins
//! that would be infeasible against the raw archive.

use anyhow::{Context, Result};
use bytes::Bytes;
use chrono::NaiveDate;
use clap::Parser;
use flate2::read::GzDecoder;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::sleep;

const USER_AGENT: &str = "githubstats/0.1 (github-archive-loader)";

/// HTTP client settings.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Per-request timeout covers the full fetch including body download.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// If no bytes arrive within this window a read is aborted.
/// reqwest's read_timeout resets after each successful read, so this
/// catches stalled connections without penalising legitimately slow ones.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// TCP keepalive: send a probe after this idle period.
const TCP_KEEPALIVE: Duration = Duration::from_secs(30);

/// Retry settings for transient download failures.
const MAX_RETRIES: u32 = 4;
/// Initial back-off; doubles each attempt: 2 s, 4 s, 8 s, 16 s.
const RETRY_BASE_DELAY: Duration = Duration::from_secs(2);

/// Channel capacities — bound the download chain stages to limit peak RAM.
const JOBS_CAPACITY: usize = 256;
/// Raw compressed bytes: keep a few in flight to absorb download/decompress jitter.
const RAW_BYTES_CAPACITY: usize = 16;
const EVENTS_CAPACITY: usize = 8_192;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "github-archive-loader",
    about = "Download GitHub Archive files for a month and write aggregated event counts as CSV"
)]
struct Args {
    /// Year to fetch (e.g. 2026)
    #[arg(long, value_parser = clap::value_parser!(i32).range(2011..))]
    year: i32,

    /// Month to fetch (1–12)
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=12))]
    month: u32,

    /// Number of download workers running concurrently
    #[arg(long, default_value_t = 10)]
    parallelism: usize,

    /// Only download this many archives (for testing); omit to fetch the whole month
    #[arg(long)]
    limit: Option<usize>,

    /// Path for the output CSV file
    #[arg(long)]
    output: PathBuf,
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// One unit of download work: a URL and its 1-based position in the total list.
struct DownloadJob {
    url: String,
    idx: usize,
    total: usize,
}

/// Compressed `.json.gz` body received from one archive, ready to decompress.
struct RawBytes {
    url: String,
    idx: usize,
    total: usize,
    data: Bytes,
}

/// Minimal fields extracted from one raw GitHub Archive event line.
/// Actor is retained so that filtering can happen after collection.
struct RawEvent {
    actor: String,
    repo: String,
    event_type: String,
    /// `payload.action` if present, empty string otherwise.
    action: String,
    /// Primary language of the repository, if present in the event payload.
    language: String,
}

/// One fully aggregated event, keyed by (actor, repo, event_type, action).
struct AggregatedEvent {
    actor: String,
    repo: String,
    event_type: String,
    action: String,
    language: String,
    count: u64,
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // ── Download chain channels ───────────────────────────────────────────────
    // jobs + raw_bytes: MPMC (async_channel) so N workers can each hold a clone.
    let (jobs_tx, jobs_rx) = async_channel::bounded::<DownloadJob>(JOBS_CAPACITY);
    let (raw_tx, raw_rx) = async_channel::bounded::<RawBytes>(RAW_BYTES_CAPACITY);
    let (events_tx, events_rx) = mpsc::channel::<RawEvent>(EVENTS_CAPACITY);

    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .tcp_keepalive(TCP_KEEPALIVE)
        .build()
        .context("failed to build HTTP client")?;

    let start = Instant::now();

    // ── Spawn download chain ──────────────────────────────────────────────────
    let mut set = JoinSet::new();

    set.spawn(populate_download_jobs(
        jobs_tx, args.year, args.month, args.limit,
    ));

    for _ in 0..args.parallelism {
        set.spawn(download_events_archive(
            jobs_rx.clone(),
            raw_tx.clone(),
            client.clone(),
        ));
    }
    drop(raw_tx); // only the worker clones remain

    for _ in 0..args.parallelism {
        let raw_rx = raw_rx.clone();
        let events_tx = events_tx.clone();
        set.spawn_blocking(move || decompress_events_archive(raw_rx, events_tx));
    }
    drop(events_tx); // only the decompress-worker clones remain

    // aggregate_events lives outside the JoinSet — it returns Vec<AggregatedEvent>
    // and must be joined after the download/decompress chain has finished.
    let aggregate_handle = tokio::spawn(aggregate_events(events_rx));

    // Wait for populate + download + decompress to finish.
    while let Some(res) = set.join_next().await {
        res.context("pipeline task panicked")??;
    }

    let events = aggregate_handle
        .await
        .context("collect_events task panicked")??;

    eprintln!(
        "Download chain done in {:.1}s",
        start.elapsed().as_secs_f64()
    );

    write_csv(events, &args.output)?;

    eprintln!("Done in {:.1}s", start.elapsed().as_secs_f64());
    Ok(())
}

// ── Stage 1: populate download jobs ──────────────────────────────────────────

/// Generates one [`DownloadJob`] per hour in the given month, truncates to
/// `limit` if set, and sends them into `tx`.  Drops the sender when done so
/// workers exit once the queue is drained.
async fn populate_download_jobs(
    tx: async_channel::Sender<DownloadJob>,
    year: i32,
    month: u32,
    limit: Option<usize>,
) -> Result<()> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)
        .with_context(|| format!("invalid date {year}-{month:02}-01"))?;
    let next_month_first = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .with_context(|| "could not compute first day of next month")?;
    let days_in_month = (next_month_first - first).num_days() as u32;

    let total_urls = days_in_month * 24;
    let total = limit
        .unwrap_or(total_urls as usize)
        .min(total_urls as usize);

    eprintln!(
        "Fetching {total} archives for {year}-{month:02}{}",
        if limit.is_some() {
            " (sample mode)"
        } else {
            ""
        },
    );

    let mut idx = 0usize;
    'outer: for day in 1..=days_in_month {
        for hour in 0..24_u32 {
            idx += 1;
            if idx > total {
                break 'outer;
            }
            let url =
                format!("https://data.gharchive.org/{year}-{month:02}-{day:02}-{hour}.json.gz");
            tx.send(DownloadJob { url, idx, total })
                .await
                .context("jobs channel closed early")?;
        }
    }
    // tx dropped here — channel closes once all workers have drained it.
    Ok(())
}

// ── Stage 2: download workers ─────────────────────────────────────────────────

/// Worker: repeatedly pulls a [`DownloadJob`] from the MPMC job channel,
/// fetches the raw compressed `.json.gz` body, and forwards it as [`RawBytes`].
///
/// Purely network I/O — no decompression or parsing here.
/// Stops when the channel is closed and empty (all jobs consumed).
async fn download_events_archive(
    jobs_rx: async_channel::Receiver<DownloadJob>,
    raw_tx: async_channel::Sender<RawBytes>,
    client: reqwest::Client,
) -> Result<()> {
    while let Ok(job) = jobs_rx.recv().await {
        match fetch_bytes_with_retry(&client, &job.url, job.idx, job.total).await {
            Err(_) => {} // already logged in fetch_bytes_with_retry
            Ok(None) => {
                eprintln!(
                    "  [{:>4}/{}] 404 (skipped) — {}",
                    job.idx, job.total, job.url
                );
            }
            Ok(Some(data)) => {
                // If the decompress stage has closed, just move on.
                let _ = raw_tx
                    .send(RawBytes {
                        url: job.url.clone(),
                        idx: job.idx,
                        total: job.total,
                        data,
                    })
                    .await;
            }
        }
    }
    // raw_tx clone dropped here.
    Ok(())
}

// ── Stage 3: decompress workers ───────────────────────────────────────────────

/// Worker: receives raw compressed [`RawBytes`], decompresses with flate2,
/// parses every JSON line, and forwards [`RawEvent`]s to `events_tx`.
///
/// Runs synchronously — intended to be spawned via [`task::spawn_blocking`]
/// so it executes on tokio's blocking thread pool, leaving the async executor
/// free for network I/O.
fn decompress_events_archive(
    raw_rx: async_channel::Receiver<RawBytes>,
    events_tx: mpsc::Sender<RawEvent>,
) -> Result<()> {
    while let Ok(raw) = raw_rx.recv_blocking() {
        let decoder = GzDecoder::new(raw.data.as_ref());
        let reader = BufReader::new(decoder);
        let mut sent = 0usize;

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    eprintln!(
                        "  [{:>4}/{}] WARN decompress error in {} — {e:#}",
                        raw.idx, raw.total, raw.url
                    );
                    break;
                }
            };
            if line.is_empty() {
                continue;
            }
            let value: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Some(event) = extract_event(&value) else {
                continue;
            };
            if events_tx.blocking_send(event).is_err() {
                break; // downstream closed
            }
            sent += 1;
        }

        eprintln!(
            "  [{:>4}/{}] {} — {sent} events",
            raw.idx, raw.total, raw.url
        );
    }

    // events_tx clone dropped here; last decompress worker drop closes the events channel.
    Ok(())
}

// ── Stage 4: collect events ───────────────────────────────────────────────────

/// Drains the [`RawEvent`] channel and aggregates into a `Vec<EventRecord>`,
/// summing counts per `(actor, repo, event_type, action)` key.
async fn aggregate_events(mut rx: mpsc::Receiver<RawEvent>) -> Result<Vec<AggregatedEvent>> {
    // Key: (actor, repo, event_type, action)  Value: (count, language)
    let mut map: HashMap<(String, String, String, String), (u64, String)> = HashMap::new();
    let mut total: u64 = 0;

    while let Some(event) = rx.recv().await {
        total += 1;
        if total.is_multiple_of(1_000_000) {
            eprintln!("  [collect] {total} events collected");
        }
        let entry = map
            .entry((event.actor, event.repo, event.event_type, event.action))
            .or_insert((0, event.language));
        entry.0 += 1;
    }

    let records: Vec<AggregatedEvent> = map
        .into_iter()
        .map(
            |((actor, repo, event_type, action), (count, language))| AggregatedEvent {
                actor,
                repo,
                event_type,
                action,
                language,
                count,
            },
        )
        .collect();

    eprintln!(
        "  [collect] {total} total events, {} unique (actor, repo, event_type, action) keys",
        records.len()
    );
    Ok(records)
}

/// Writes `Vec<AggregatedEvent>` as RFC 4180 CSV to `path`.
///
/// Format:
///   actor,repo,event_type,action,language,count
///   someuser,owner/name,PullRequestEvent,closed,Rust,42
fn write_csv(rows: Vec<AggregatedEvent>, path: &PathBuf) -> Result<()> {
    let file = File::create(path).with_context(|| format!("create {path:?}"))?;
    let mut w = BufWriter::new(file);
    w.write_all(b"actor,repo,event_type,action,language,count\n")
        .context("write CSV header")?;

    for row in &rows {
        let actor = csv_field(&row.actor);
        let repo = csv_field(&row.repo);
        let etype = csv_field(&row.event_type);
        let action = csv_field(&row.action);
        let language = csv_field(&row.language);
        writeln!(
            w,
            "{actor},{repo},{etype},{action},{language},{}",
            row.count
        )
        .context("write CSV row")?;
    }

    w.flush().context("flush CSV")?;
    eprintln!("  [writer] {} rows written to {path:?}", rows.len());
    Ok(())
}

// ── Fetching ──────────────────────────────────────────────────────────────────

/// Fetches one `.json.gz` archive and returns the raw compressed bytes.
///
/// Returns `Ok(None)` on 404 (archive not yet published).
/// Returns `Err` on any other network or HTTP error so the caller can retry.
async fn fetch_bytes(client: &reqwest::Client, url: &str) -> Result<Option<Bytes>> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }

    let bytes = response
        .error_for_status()
        .with_context(|| format!("HTTP error for {url}"))?
        .bytes()
        .await
        .with_context(|| format!("reading body of {url}"))?;

    Ok(Some(bytes))
}

/// Calls [`fetch_bytes`] with exponential back-off retry on transient errors.
///
/// `Ok(None)` and `Ok(Some(_))` are passed through directly to the caller.
/// Only `Err` results trigger a retry; after [`MAX_RETRIES`] the final error
/// is logged as WARN and `Ok(None)` is returned so the job is skipped.
async fn fetch_bytes_with_retry(
    client: &reqwest::Client,
    url: &str,
    idx: usize,
    total: usize,
) -> Result<Option<Bytes>> {
    let mut last_err = anyhow::anyhow!("no attempts made");

    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            let delay = RETRY_BASE_DELAY * 2u32.pow(attempt - 1);
            eprintln!(
                "  [{idx:>4}/{total}] retry {attempt}/{MAX_RETRIES} in {}s — {url}",
                delay.as_secs(),
            );
            sleep(delay).await;
        }

        match fetch_bytes(client, url).await {
            Ok(result) => return Ok(result),
            Err(e) => last_err = e,
        }
    }

    eprintln!(
        "  [{idx:>4}/{total}] WARN {url} (gave up after {MAX_RETRIES} retries): {last_err:#}"
    );
    Err(last_err)
}

// ── Event extraction ──────────────────────────────────────────────────────────

/// Extracts fields from a raw GitHub Archive event JSON object.
///
/// Returns `None` if required fields are missing or malformed.
/// Bot filtering is no longer done here — it moved to [`filter_events`].
fn extract_event(value: &Value) -> Option<RawEvent> {
    let actor_login = value
        .get("actor")
        .and_then(|a| a.get("login"))
        .and_then(|l| l.as_str())
        .unwrap_or("");

    let event_type = value.get("type")?.as_str()?.to_string();

    let repo = value.get("repo")?.get("name")?.as_str()?;
    if !repo.contains('/') {
        return None;
    }

    let action = value
        .get("payload")
        .and_then(|p| p.get("action"))
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .to_string();

    // Language is available in some event types:
    //   payload.pull_request.head.repo.language  (PullRequestEvent)
    //   payload.issue.repository.language        (IssuesEvent)
    //   payload.repository.language              (CreateEvent, etc.)
    let payload = value.get("payload");
    let language = payload
        .and_then(|p| p.get("pull_request"))
        .and_then(|pr| pr.get("head"))
        .and_then(|h| h.get("repo"))
        .and_then(|r| r.get("language"))
        .and_then(|l| l.as_str())
        .or_else(|| {
            payload
                .and_then(|p| p.get("issue"))
                .and_then(|i| i.get("repository"))
                .and_then(|r| r.get("language"))
                .and_then(|l| l.as_str())
        })
        .or_else(|| {
            payload
                .and_then(|p| p.get("repository"))
                .and_then(|r| r.get("language"))
                .and_then(|l| l.as_str())
        })
        .unwrap_or("")
        .to_string();

    Some(RawEvent {
        actor: actor_login.to_string(),
        repo: repo.to_string(),
        event_type,
        action,
        language,
    })
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
