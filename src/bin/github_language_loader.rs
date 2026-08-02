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
//! Internal pipeline (sequential — one request at a time):
//!   produce_batches   reads stdin → Vec<Vec<String>>
//!   download_one      one GraphQL batch request → BatchOutcome
//!   write + log       write JSONL to stdout (flushed every batch), progress to stderr
//!   sleep             adaptive cooldown between requests
//!
//! Rate-limit strategy
//! -------------------
//! GitHub enforces two independent limits:
//!   1. **Primary limit** — 5000 GraphQL points/hour, refilled continuously.
//!      Exhausting it forces a wait until the next reset.  This is the hard
//!      throughput ceiling and cannot be worked around; it is only honoured.
//!   2. **Secondary ("abuse") limit** — a sliding-window burst detector that
//!      returns HTTP 403/429 with a `retry-after` header.  It is triggered by
//!      frequent and/or expensive requests and is the one that bites most
//!      often on long batch runs.
//!
//! Two mechanisms keep the secondary limit at bay:
//!   * **Adaptive pacing** — the inter-batch `cooldown` self-tunes.  It starts
//!     at 0, grows by 1 s on every secondary-rate-limit hit (reacting fast to
//!     protect the run), and shrinks by 1 s only after a long run of clean
//!     batches (so it relaxes slowly and never overshoots the true ceiling).
//!     This lets the loader find a sustainable request rate automatically
//!     instead of relying on a hard-coded wait that is either too aggressive
//!     (constant 60 s penalties) or too conservative (wasted idle time).
//!   * **Smaller batches** — the abuse detector appears to flag large batched
//!     `repository(...)` queries.  `--batch-size` lets you trade request count
//!     for per-request complexity; smaller values are far less likely to trip
//!     the secondary limit.
//!
//! Resumability
//! ------------
//! Per-batch stdout flushing means a killed run leaves a usable partial
//! output.  Callers are responsible for not re-requesting already-fetched
//! repos — `collect_month.sh`, for example, diffs the existing
//! `languages-YYYY-MM.jsonl` against the archive's repo list on the host and
//! pipes only the still-pending slugs into stdin.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use humanize_duration::Truncate;
use humanize_duration::prelude::DurationExt;
use serde::Serialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};

/// Maximum repositories per GraphQL request (overridable via `--batch-size`).
const DEFAULT_BATCH_SIZE: usize = 100;
const MAX_RETRIES: u32 = 3;
const RETRY_WAIT: Duration = Duration::from_secs(5);
const USER_AGENT: &str = "githubstats/0.1 (https://github.com/guenhter/githubstat)";

/// Adaptive-cooldown step.  Each secondary-rate-limit hit grows the inter-batch
/// wait by this much; the wait decays by the same amount after a long run of
/// clean batches.  Reacts fast (rises immediately), relaxes slowly.
const COOLDOWN_STEP: Duration = Duration::from_secs(1);
/// Number of consecutive clean batches required before the cooldown shrinks by
/// one step.  Large on purpose: the secondary limit is a sliding-window burst
/// detector, so a long clean run is the only reliable signal that the window
/// has cleared and it is safe to go faster.
const CLEAN_STREAK_FOR_DECAY: u32 = 200;

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

    /// Repositories per GraphQL request.  Smaller batches are cheaper per
    /// request and far less likely to trip GitHub's secondary ("abuse")
    /// rate limit, at the cost of more total requests.
    #[arg(long, default_value_t = DEFAULT_BATCH_SIZE)]
    batch_size: usize,
}

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Serialize)]
struct RepoLanguages {
    repo: String,
    total_size: u64,
    languages: Vec<LanguageEntry>,
    /// UTC ISO-8601 timestamp (second precision) when this repo's languages
    /// were fetched from the GitHub GraphQL API.
    fetched_at: String,
}

#[derive(Clone, Serialize)]
struct LanguageEntry {
    language: String,
    size: u64,
}

/// The full output of one completed batch — written to stdout and logged to stderr.
#[derive(Clone)]
struct BatchOutcome {
    /// Language results to be written to stdout (includes repos with empty language lists).
    languages: Vec<RepoLanguages>,
    /// GraphQL rate-limit snapshot: (cost, remaining).
    rate_limit: Option<(i64, i64)>,
    /// Wall-clock time spent on this batch's GraphQL round-trip (incl. in-call waits).
    elapsed: Duration,
    /// Number of secondary-rate-limit (403/429) hits encountered while serving this batch.
    secondary_hits: u32,
    /// Number of primary-rate-limit waits encountered while serving this batch.
    primary_waits: u32,
}

/// Configuration carried through the pipeline (cheaply cloneable).
#[derive(Clone)]
struct WorkerConfig {
    client: reqwest::Client,
    token: String,
    max_languages: usize,
    batch_size: usize,
}

// ── Adaptive pacing ───────────────────────────────────────────────────────────

/// Self-tuning inter-batch wait.  Starts at zero, grows by `COOLDOWN_STEP` on
/// every secondary-rate-limit hit (reacting fast to protect the run), and
/// shrinks by one step only after a long run of clean batches (so it relaxes
/// slowly and never overshoots the true sustainable rate).
struct Cooldown {
    current: Duration,
    clean_streak: u32,
}

impl Cooldown {
    fn new() -> Self {
        Self {
            current: Duration::ZERO,
            clean_streak: 0,
        }
    }

    /// Adjust the cooldown from the outcome of the just-completed batch.
    fn update(&mut self, outcome: &BatchOutcome) {
        if outcome.secondary_hits > 0 {
            self.clean_streak = 0;
            self.current = self.current.saturating_add(COOLDOWN_STEP);
        } else {
            self.clean_streak = self.clean_streak.saturating_add(1);
            if self.clean_streak >= CLEAN_STREAK_FOR_DECAY && !self.current.is_zero() {
                self.current = self.current.saturating_sub(COOLDOWN_STEP);
                self.clean_streak = 0; // streak consumed; require another full run for the next decay
            }
        }
    }

    /// Sleep for the current cooldown between batches.
    async fn wait(&self) {
        if !self.current.is_zero() {
            tokio::time::sleep(self.current).await;
        }
    }
}

// ── Progress logging ──────────────────────────────────────────────────────────

/// Accumulates run counters and renders one progress line per batch to stderr.
struct Progress {
    total_batches: usize,
    total_repos: usize,
    run_start: Instant,
    batches_done: usize,
    repos_done: usize,
    written: u64,
    prev_remaining: Option<i64>,
}

impl Progress {
    fn new(total_batches: usize, total_repos: usize) -> Self {
        Self {
            total_batches,
            total_repos,
            run_start: Instant::now(),
            batches_done: 0,
            repos_done: 0,
            written: 0,
            prev_remaining: None,
        }
    }

    /// Advance counters and print a single progress line for `outcome`.
    fn tick(&mut self, outcome: &BatchOutcome, cooldown: &Cooldown) {
        self.repos_done += outcome.languages.len();
        self.batches_done += 1;

        let rl = outcome
            .rate_limit
            .map(|(c, r)| {
                let delta = self.prev_remaining.map(|p| p - r).unwrap_or(0);
                self.prev_remaining = Some(r);
                format!("  [rl cost={c} remaining={r} Δ={delta}]")
            })
            .unwrap_or_default();
        let sec = if outcome.secondary_hits > 0 {
            format!("  [secondary×{}]", outcome.secondary_hits)
        } else {
            String::new()
        };
        let pri = if outcome.primary_waits > 0 {
            format!("  [primary×{}]", outcome.primary_waits)
        } else {
            String::new()
        };
        let eta = eta_string(self.run_start, self.batches_done, self.total_batches);
        eprintln!(
            "[{done}/{total_batches} repos={repos_done}/{total_repos} written={written}]{sec}{pri}{rl}  cd={cd_ms}ms  [{elapsed}]  ETA {eta}",
            done = self.batches_done,
            total_batches = self.total_batches,
            repos_done = self.repos_done,
            total_repos = self.total_repos,
            written = self.written,
            sec = sec,
            pri = pri,
            rl = rl,
            cd_ms = cooldown.current.as_millis(),
            elapsed = outcome.elapsed.human(Truncate::Millis),
        );
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config = build_config(args)?;
    let batches = produce_batches(config.batch_size).await?;
    let total_repos: usize = batches.iter().map(Vec::len).sum();

    eprintln!(
        "[start] {} batches ({} repos) at batch_size={}",
        batches.len(),
        total_repos,
        config.batch_size,
    );

    let mut writer = BufWriter::new(tokio::io::stdout());
    let mut cooldown = Cooldown::new();
    let mut progress = Progress::new(batches.len(), total_repos);

    for batch in batches {
        let outcome = download_one(batch, &config).await;
        progress.written += write_batch(&mut writer, &outcome).await?;
        cooldown.update(&outcome);
        progress.tick(&outcome, &cooldown);
        cooldown.wait().await;
    }

    eprintln!(
        "\nDone. {} entries written to stdout in {}",
        progress.written,
        progress.run_start.elapsed().human(Truncate::Second)
    );
    Ok(())
}

/// Build the HTTP client and worker config from CLI args and the environment.
fn build_config(args: Args) -> Result<WorkerConfig> {
    let token = std::env::var("GITHUB_TOKEN").context("GITHUB_TOKEN is not set")?;
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .context("failed to build HTTP client")?;
    Ok(WorkerConfig {
        client,
        token,
        max_languages: args.max_languages,
        batch_size: args.batch_size,
    })
}

/// Append one batch's non-empty language entries to the output stream as JSONL.
/// Returns the number of entries written.  Flushes after each batch so a killed
/// run leaves a usable partial output on disk.
async fn write_batch(
    writer: &mut (impl AsyncWriteExt + Unpin),
    outcome: &BatchOutcome,
) -> Result<u64> {
    let mut written = 0;
    for entry in &outcome.languages {
        if entry.languages.is_empty() {
            continue;
        }
        let line = serde_json::to_string(entry).context("serialise")?;
        writer.write_all(line.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        written += 1;
    }
    writer.flush().await?;
    Ok(written)
}

/// Rough wall-clock ETA for the remaining batches based on observed throughput so far.
fn eta_string(start: Instant, done: usize, total: usize) -> String {
    if done == 0 || total == 0 {
        return String::from("—");
    }
    let elapsed = start.elapsed();
    let per_batch = elapsed / done as u32;
    let remaining = total.saturating_sub(done);
    let eta = per_batch * remaining as u32;
    eta.human(Truncate::Second).to_string()
}

// ── Pipeline stages ───────────────────────────────────────────────────────────

/// Stage 1: read stdin line by line and assemble batches.
/// Returns all batches so the caller knows the total count before processing starts.
async fn produce_batches(batch_size: usize) -> Result<Vec<Vec<String>>> {
    let mut batches: Vec<Vec<String>> = Vec::new();
    let mut buffer: Vec<String> = Vec::with_capacity(batch_size);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await.context("failed to read stdin")? {
        let line = line.trim().to_string();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(slug) = normalize_repo(&line) else {
            eprintln!("  [skip] unrecognised input line: {line}");
            continue;
        };
        buffer.push(slug.to_string());
        if buffer.len() == batch_size {
            batches.push(std::mem::take(&mut buffer));
            buffer = Vec::with_capacity(batch_size);
        }
    }
    if !buffer.is_empty() {
        batches.push(buffer);
    }
    Ok(batches)
}

// ── Worker ────────────────────────────────────────────────────────────────────

/// Query GraphQL for one batch and return a BatchOutcome.
/// Rate-limit waits and retries are handled here; errors are reflected in the outcome.
async fn download_one(batch: Vec<String>, config: &WorkerConfig) -> BatchOutcome {
    let refs: Vec<&str> = batch.iter().map(String::as_str).collect();
    let query = build_languages_query(&refs, config.max_languages);
    let t0 = Instant::now();
    let mut secondary_hits = 0u32;
    let mut primary_waits = 0u32;

    // Capture the fetch timestamp once per batch — all repos in a batch share
    // one GraphQL round-trip, so they share one ingestion time.
    let fetched_at = utc_now_iso();

    let resp = match call_graphql(
        &config.client,
        &query,
        &config.token,
        &mut secondary_hits,
        &mut primary_waits,
    )
    .await
    {
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
                        fetched_at: fetched_at.clone(),
                    })
                    .collect(),
                rate_limit: None,
                elapsed: t0.elapsed(),
                secondary_hits,
                primary_waits,
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

    let languages: Vec<RepoLanguages> = extract_languages(&resp, &refs, &fetched_at)
        .into_iter()
        .map(|(_, repo_languages)| repo_languages)
        .collect();

    BatchOutcome {
        languages,
        rate_limit: extract_rate_limit(&resp),
        elapsed: t0.elapsed(),
        secondary_hits,
        primary_waits,
    }
}

// ── GraphQL ───────────────────────────────────────────────────────────────────

async fn call_graphql(
    client: &reqwest::Client,
    query: &str,
    token: &str,
    secondary_hits: &mut u32,
    primary_waits: &mut u32,
) -> Result<Value> {
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
        if let Some((wait, kind)) = rate_limit_wait(&resp) {
            match kind {
                LimitKind::Secondary => *secondary_hits += 1,
                LimitKind::Primary => *primary_waits += 1,
            }
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

enum LimitKind {
    Secondary,
    Primary,
}

/// Returns `Some((wait, kind))` and logs a message when the response signals rate limiting.
/// Returns `None` when the response body can be consumed normally.
fn rate_limit_wait(resp: &reqwest::Response) -> Option<(Duration, LimitKind)> {
    let status = resp.status().as_u16();

    // Secondary rate limit: 403/429 — honour retry-after or fall back to 60 s.
    if status == 403 || status == 429 {
        let secs = header_u64(resp, "retry-after").unwrap_or(60);
        eprintln!("  [rate-limit] secondary limit (HTTP {status}): waiting {secs}s …");
        return Some((Duration::from_secs(secs), LimitKind::Secondary));
    }

    // Primary rate limit exhausted: x-ratelimit-remaining == 0.
    if header_u64(resp, "x-ratelimit-remaining") == Some(0) {
        let reset = header_u64(resp, "x-ratelimit-reset").unwrap_or(0);
        let wait = secs_until(reset) + Duration::from_secs(1); // +1 s buffer
        eprintln!(
            "  [rate-limit] primary limit exhausted: waiting {}s until reset …",
            wait.as_secs()
        );
        return Some((wait, LimitKind::Primary));
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

/// Current UTC time as an ISO-8601 string with second precision (e.g. `2026-08-02T17:30:45Z`).
fn utc_now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn extract_rate_limit(resp: &Value) -> Option<(i64, i64)> {
    let rl = resp.pointer("/data/rateLimit")?;
    Some((rl.get("cost")?.as_i64()?, rl.get("remaining")?.as_i64()?))
}

fn extract_languages(
    resp: &Value,
    repos: &[&str],
    fetched_at: &str,
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
            Some((
                repo.to_string(),
                RepoLanguages {
                    repo: repo.to_string(),
                    total_size,
                    languages: entries,
                    fetched_at: fetched_at.to_string(),
                },
            ))
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
    Some(LanguageEntry {
        language: name,
        size,
    })
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
