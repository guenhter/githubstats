//! github-language-loader
//!
//! Reads "owner/name" repository slugs from stdin and queries the GitHub
//! GraphQL API to fetch the language breakdown for each repository.
//!
//! Input format (stdin):
//!   - One slug per line: `owner/name`
//!   - Full GitHub URLs are also accepted: `https://github.com/owner/name`
//!   - Blank lines and lines starting with `#` are ignored
//!
//! Output — stdout (JSONL, one object per line):
//!   {"repo":"rust-lang/rust","languages":[{"language":"Rust","percent":92.3},…]}
//!   {"repo":"torvalds/linux","languages":[{"language":"C","percent":97.6},…]}
//!
//! All progress and diagnostic messages are written to stderr so that stdout
//! remains a clean JSONL stream safe to pipe or redirect.
//!
//! Internal pipeline (fully async via Tokio):
//!   produce_batches  reads stdin → batch channel
//!   load_languages   batch channel → worker tasks → result broadcast
//!   write_results    result broadcast → stdout JSONL
//!   log_results      result broadcast → stderr progress

use anyhow::{Context, Result};
use clap::Parser;
use humanize_duration::prelude::DurationExt;
use humanize_duration::Truncate;
use serde::Serialize;
use serde_json::{json, Value};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::broadcast;
use tokio::task::JoinSet;

/// Maximum repositories per GraphQL request.
const BATCH_SIZE: usize = 100;
const MAX_RETRIES: u32 = 3;
const RETRY_WAIT: Duration = Duration::from_secs(5);
const USER_AGENT: &str = "githubstats/0.1 (https://github.com/guenhter/githubstat)";
/// Broadcast channel capacity — sized well above the maximum number of in-flight batches.
const BROADCAST_CAPACITY: usize = 512;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "github-language-loader",
    about = "Fetch language breakdowns for GitHub repos from stdin; emits JSONL on stdout"
)]
struct Args {
    /// Maximum number of languages to fetch per repo from GitHub (ordered by size, largest first)
    #[arg(long, default_value_t = 5)]
    max_languages: usize,

    /// Number of concurrent language-loader workers
    #[arg(long, default_value_t = 4)]
    workers: usize,
}

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Serialize)]
struct RepoLanguages {
    repo: String,
    total_size: u64,
    languages: Vec<LanguageEntry>,
}

#[derive(Clone, Serialize)]
struct LanguageEntry {
    language: String,
    size: u64,
}

/// The full output of one completed worker batch, broadcast to writer and logger.
#[derive(Clone)]
struct BatchOutcome {
    /// Language results to be written to stdout (includes repos with empty language lists).
    languages: Vec<RepoLanguages>,
    /// GraphQL rate-limit snapshot: (cost, remaining).
    rate_limit: Option<(i64, i64)>,
    /// Wall-clock time spent on this batch's GraphQL round-trip.
    elapsed: Duration,
}

/// Configuration shared across worker tasks (cheaply cloneable).
#[derive(Clone)]
struct WorkerConfig {
    client: reqwest::Client,
    token: String,
    max_languages: usize,
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token = std::env::var("GITHUB_TOKEN").context("GITHUB_TOKEN is not set")?;
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .context("failed to build HTTP client")?;

    let config = WorkerConfig {
        client,
        token,
        max_languages: args.max_languages,
    };

    // Stage 1: read all stdin synchronously before spawning workers so we know
    // the total batch count up front for progress logging.
    let batches = produce_batches().await?;
    let total_batches = batches.len();

    let (batch_tx, batch_rx) = async_channel::bounded::<Vec<String>>(total_batches.max(1));
    for batch in batches {
        batch_tx.send(batch).await.context("batch channel send")?;
    }
    drop(batch_tx); // channel is fully populated; dropping signals workers to stop when drained

    let (result_tx, _) = broadcast::channel::<BatchOutcome>(BROADCAST_CAPACITY);

    let mut loaders: JoinSet<Result<()>> = JoinSet::new();
    for _ in 0..args.workers {
        loaders.spawn(load_languages(
            batch_rx.clone(),
            result_tx.clone(),
            config.clone(),
        ));
    }
    let writer = tokio::spawn(write_results(result_tx.subscribe()));
    let logger = tokio::spawn(log_results(result_tx.subscribe(), total_batches));

    loaders.join_all().await;

    drop(result_tx); // close broadcast — signals writer and logger to finish

    let written = writer.await??;
    logger.await?;

    eprintln!("\nDone. {written} entries written to stdout");
    Ok(())
}

// ── Pipeline stages ───────────────────────────────────────────────────────────

/// Stage 1: read stdin line by line and assemble batches.
/// Returns all batches so the caller knows the total count before workers start.
async fn produce_batches() -> Result<Vec<Vec<String>>> {
    let mut batches: Vec<Vec<String>> = Vec::new();
    let mut buffer: Vec<String> = Vec::with_capacity(BATCH_SIZE);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await.context("failed to read stdin")? {
        let line = line.trim().to_string();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(slug) = normalize_repo(&line) {
            buffer.push(slug.to_string());
            if buffer.len() == BATCH_SIZE {
                batches.push(std::mem::take(&mut buffer));
                buffer = Vec::with_capacity(BATCH_SIZE);
            }
        } else {
            eprintln!("  [skip] unrecognised input line: {line}");
        }
    }
    if !buffer.is_empty() {
        batches.push(buffer);
    }
    Ok(batches)
}

/// Stage 2: one of N concurrent workers — pulls batches from the shared MPMC queue,
/// processes them one at a time, and broadcasts each BatchOutcome.
/// The worker exits when the channel is closed and drained.
async fn load_languages(
    batch_rx: async_channel::Receiver<Vec<String>>,
    result_tx: broadcast::Sender<BatchOutcome>,
    config: WorkerConfig,
) -> Result<()> {
    while let Ok(batch) = batch_rx.recv().await {
        let outcome = download_one(batch, &config).await;
        let _ = result_tx.send(outcome);
    }
    Ok(())
}

// ── Sinks ────────────────────────────────────────────────────────────────────

/// Stage 3: receive every BatchOutcome and write JSONL to stdout.
async fn write_results(mut rx: broadcast::Receiver<BatchOutcome>) -> Result<u64> {
    let mut writer = BufWriter::new(tokio::io::stdout());
    let mut count = 0u64;
    loop {
        match rx.recv().await {
            Ok(outcome) => {
                for entry in &outcome.languages {
                    if entry.languages.is_empty() {
                        continue;
                    }
                    let line = serde_json::to_string(entry).context("serialise")?;
                    writer.write_all(line.as_bytes()).await?;
                    writer.write_all(b"\n").await?;
                    count += 1;
                }
            }
            Err(broadcast::error::RecvError::Closed) => break,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                eprintln!("  [writer] warning: {n} outcomes skipped due to lag — output may be incomplete");
            }
        }
    }
    writer.flush().await?;
    Ok(count)
}

/// Stage 4: receive every BatchOutcome and print a progress line to stderr.
async fn log_results(mut rx: broadcast::Receiver<BatchOutcome>, total_batches: usize) {
    let total_repos = total_batches * BATCH_SIZE; // upper bound; last batch may be smaller
    let mut repos_done: usize = 0;
    let mut batches_done: usize = 0;
    loop {
        match rx.recv().await {
            Ok(outcome) => {
                repos_done += outcome.languages.len();
                batches_done += 1;
                let rl = outcome
                    .rate_limit
                    .map(|(c, r)| format!("  [rate-limit: cost={c}/remaining={r}]"))
                    .unwrap_or_default();
                eprintln!(
                    "[repos={repos_done} / batch {batches_done}/{total_batches} (~{total_repos} repos)]{rl}  [{}]",
                    outcome.elapsed.human(Truncate::Millis)
                );
            }
            Err(broadcast::error::RecvError::Closed) => break,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                eprintln!("  [logger] skipped {n} progress lines due to lag");
            }
        }
    }
}

// ── Worker ────────────────────────────────────────────────────────────────────

/// Query GraphQL for one batch and return a BatchOutcome.
/// Rate-limit waits and retries are handled here; errors are reflected in the outcome.
async fn download_one(batch: Vec<String>, config: &WorkerConfig) -> BatchOutcome {
    let refs: Vec<&str> = batch.iter().map(String::as_str).collect();
    let query = build_languages_query(&refs, config.max_languages);
    let t0 = Instant::now();

    let resp = match call_graphql(&config.client, &query, &config.token).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  [SKIP] batch failed entirely: {e}");
            return BatchOutcome {
                languages: refs
                    .iter()
                    .map(|s| RepoLanguages {
                        repo: s.to_string(),
                        total_size: 0,
                        languages: vec![],
                    })
                    .collect(),
                rate_limit: None,
                elapsed: t0.elapsed(),
            };
        }
    };

    if let Some(errors) = resp.get("errors").and_then(|e| e.as_array()) {
        let other_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.get("type").and_then(|t| t.as_str()) != Some("NOT_FOUND"))
            .collect();
        if !other_errors.is_empty() {
            eprintln!(
                "  [GraphQL errors]: {}",
                serde_json::to_string(&other_errors).unwrap_or_default()
            );
        }
    }

    let languages: Vec<RepoLanguages> = extract_languages(&resp, &refs)
        .into_iter()
        .map(|(_, repo_languages)| repo_languages)
        .collect();

    BatchOutcome {
        languages,
        rate_limit: extract_rate_limit(&resp),
        elapsed: t0.elapsed(),
    }
}

// ── GraphQL ───────────────────────────────────────────────────────────────────

async fn call_graphql(client: &reqwest::Client, query: &str, token: &str) -> Result<Value> {
    let mut attempts = 0u32;
    loop {
        let resp = match send_graphql_request(client, query, token).await {
            Ok(r) => r,
            Err(e) if attempts < MAX_RETRIES => {
                attempts += 1;
                eprintln!(
                    "  [retry {attempts}/{MAX_RETRIES}] request error: {e:#} — retrying in {}s …",
                    RETRY_WAIT.as_secs()
                );
                tokio::time::sleep(RETRY_WAIT).await;
                continue;
            }
            Err(e) => return Err(e),
        };
        if let Some(wait) = rate_limit_wait(&resp) {
            drop(resp);
            tokio::time::sleep(wait).await;
            continue;
        }
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(200).collect();
            if attempts < MAX_RETRIES {
                attempts += 1;
                let wait = RETRY_WAIT * 2u32.pow(attempts - 1); // 5s, 10s, 20s
                eprintln!(
                    "  [retry {attempts}/{MAX_RETRIES}] HTTP {status}: {snippet} — retrying in {}s …",
                    wait.as_secs()
                );
                tokio::time::sleep(wait).await;
                continue;
            }
            return Err(anyhow::anyhow!("HTTP {status}: {snippet}"));
        }
        return resp
            .json::<Value>()
            .await
            .context("GraphQL response was not valid JSON");
    }
}

async fn send_graphql_request(
    client: &reqwest::Client,
    query: &str,
    token: &str,
) -> Result<reqwest::Response> {
    client
        .post("https://api.github.com/graphql")
        .header("Authorization", format!("Bearer {token}"))
        .json(&json!({ "query": query }))
        .send()
        .await
        .context("GraphQL request failed")
}

/// Returns `Some(wait)` and logs a message when the response signals rate limiting.
/// Returns `None` when the response body can be consumed normally.
fn rate_limit_wait(resp: &reqwest::Response) -> Option<Duration> {
    let status = resp.status().as_u16();

    // Secondary rate limit: 403/429 — honour retry-after or fall back to 60 s.
    if status == 403 || status == 429 {
        let secs = header_u64(resp, "retry-after").unwrap_or(60);
        eprintln!("  [rate-limit] secondary limit (HTTP {status}): waiting {secs}s …");
        return Some(Duration::from_secs(secs));
    }

    // Primary rate limit exhausted: x-ratelimit-remaining == 0.
    if header_u64(resp, "x-ratelimit-remaining") == Some(0) {
        let reset = header_u64(resp, "x-ratelimit-reset").unwrap_or(0);
        let wait = secs_until(reset) + Duration::from_secs(1); // +1 s buffer
        eprintln!(
            "  [rate-limit] primary limit exhausted: waiting {}s until reset …",
            wait.as_secs()
        );
        return Some(wait);
    }

    None
}

fn header_u64(resp: &reqwest::Response, name: &str) -> Option<u64> {
    resp.headers().get(name)?.to_str().ok()?.parse().ok()
}

fn secs_until(epoch: u64) -> Duration {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Duration::from_secs(epoch.saturating_sub(now))
}

fn extract_rate_limit(resp: &Value) -> Option<(i64, i64)> {
    let rl = resp.pointer("/data/rateLimit")?;
    Some((rl.get("cost")?.as_i64()?, rl.get("remaining")?.as_i64()?))
}

fn extract_languages(
    resp: &Value,
    repos: &[&str],
) -> Vec<(String, RepoLanguages)> {
    let data = match resp.get("data").and_then(|d| d.as_object()) {
        Some(d) => d,
        None => return vec![],
    };
    repos
        .iter()
        .enumerate()
        .filter_map(|(i, &repo)| {
            let node = data.get(&format!("r{i}"))?;
            let (total_size, entries) = if node.is_null() {
                (0, vec![]) // NOT_FOUND — include repo with empty languages
            } else {
                parse_language_entries(Some(node))
            };
            Some((repo.to_string(), RepoLanguages {
                repo: repo.to_string(),
                total_size,
                languages: entries,
            }))
        })
        .collect()
}

// ── Language parsing ──────────────────────────────────────────────────────────

fn parse_language_entries(node: Option<&Value>) -> (u64, Vec<LanguageEntry>) {
    let lang_node = match node.and_then(|v| v.get("languages")) {
        Some(l) => l,
        None => return (0, vec![]),
    };
    let total_size = lang_node
        .get("totalSize")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let edges = match lang_node.get("edges").and_then(|e| e.as_array()) {
        Some(e) => e,
        None => return (total_size, vec![]),
    };
    let entries = edges.iter().filter_map(edge_to_entry).collect();
    (total_size, entries)
}

fn edge_to_entry(edge: &Value) -> Option<LanguageEntry> {
    let size = edge.get("size")?.as_u64()?;
    let name = edge.get("node")?.get("name")?.as_str()?.to_string();
    Some(LanguageEntry { language: name, size })
}

/// Build a batched GraphQL query that aliases each repo as r0…rN.
fn build_languages_query(repos: &[&str], max_languages: usize) -> String {
    let fragments: Vec<String> = repos
        .iter()
        .enumerate()
        .filter_map(|(i, repo)| {
            let (owner, name) = repo.split_once('/')?;
            let owner = owner.replace('"', "");
            let name = name.replace('"', "");
            Some(format!(
                r#"r{i}: repository(owner: "{owner}", name: "{name}") {{
  languages(first: {max_languages}, orderBy: {{field: SIZE, direction: DESC}}) {{
    totalSize edges {{ size node {{ name }} }}
  }}
}}"#
            ))
        })
        .collect();
    format!(
        "{{ rateLimit {{ cost remaining }} {} }}",
        fragments.join("\n")
    )
}

// ── URL normalisation ─────────────────────────────────────────────────────────

/// Convert a GitHub URL or bare slug to an "owner/name" slug.
/// Returns `None` for strings that cannot be parsed as a GitHub repo reference.
fn normalize_repo(s: &str) -> Option<&str> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let s = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("http://github.com/"))
        .or_else(|| s.strip_prefix("github.com/"))
        .unwrap_or(s);
    let s = s.strip_suffix(".git").unwrap_or(s);
    let s = s.trim_end_matches('/');
    if s.chars().filter(|&c| c == '/').count() != 1 {
        return None;
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::normalize_repo;

    #[test]
    fn test_normalize_url() {
        assert_eq!(
            normalize_repo("https://github.com/rust-lang/rust"),
            Some("rust-lang/rust")
        );
    }

    #[test]
    fn test_normalize_slug() {
        assert_eq!(normalize_repo("torvalds/linux"), Some("torvalds/linux"));
    }

    #[test]
    fn test_normalize_git_suffix() {
        assert_eq!(
            normalize_repo("https://github.com/owner/repo.git"),
            Some("owner/repo")
        );
    }

    #[test]
    fn test_normalize_invalid() {
        assert_eq!(normalize_repo("not-a-repo"), None);
    }
}
