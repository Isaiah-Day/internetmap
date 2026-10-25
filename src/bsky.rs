use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::Arc;
use std::time::Duration;

use atrium_api::agent::atp_agent::{AtpAgent, store::MemorySessionStore};
use atrium_api::app::bsky::graph::get_follows;
use atrium_api::types::LimitedNonZeroU8;
use atrium_api::types::string::AtIdentifier;
use atrium_xrpc_client::reqwest::ReqwestClient;
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep_until};

/// Public, unauthenticated AppView. It is cached by Bluesky and is the
/// recommended endpoint for public-web use cases.
pub const PUBLIC_ENDPOINT: &str = "https://public.api.bsky.app";

pub const FOLLOWS_PATH: &str = "follows.jsonl";
pub const DONE_PATH: &str = "follows_done.txt";
pub const FAILED_PATH: &str = "follows_failed.txt";
pub const HANDLES_PATH: &str = "handles.jsonl";

/// Concurrent in-flight requests. The global `RateLimiter` is the real throttle.
pub const CONCURRENCY: usize = 8;
/// AppView allows roughly 3000 requests / 5 minutes per IP (~10/sec).
pub const REQUESTS_PER_SEC: f64 = 10.0;
/// Cap per actor: 10 pages * 100 = 1,000 followees.
pub const MAX_PAGES: usize = 10;

/// Page size accepted by `app.bsky.graph.getFollows` (max 100).
const PAGE_LIMIT: u8 = 100;

pub type PublicAgent = AtpAgent<MemorySessionStore, ReqwestClient>;

/// A lightweight reference to an account, used for graph nodes.
#[derive(Debug, Clone)]
pub struct ActorRef {
    pub did: String,
    pub handle: String,
}

/// The result of enumerating every account `subject` follows.
#[derive(Debug, Clone)]
pub struct FollowingPage {
    /// The account whose following list this is (the follower).
    pub subject: ActorRef,
    /// Accounts that `subject` follows (the followees).
    pub followees: Vec<ActorRef>,
}

/// Simple global rate limiter that spaces request *starts* at a fixed interval.
/// Shared across tasks so concurrency never exceeds the configured req/sec.
pub struct RateLimiter {
    next: Mutex<Instant>,
    interval: Duration,
}

impl RateLimiter {
    pub fn new(requests_per_second: f64) -> Self {
        Self {
            next: Mutex::new(Instant::now()),
            interval: Duration::from_secs_f64(1.0 / requests_per_second),
        }
    }

    /// Blocks until this caller is allowed to start its requests.
    pub async fn wait(&self) {
        let mut next = self.next.lock().await;
        let now = Instant::now();
        if *next > now {
            sleep_until(*next).await;
        }
        *next = Instant::now() + self.interval;
    }
}

fn page_limit() -> LimitedNonZeroU8<100> {
    LimitedNonZeroU8::<100>::try_from(PAGE_LIMIT).expect("PAGE_LIMIT must be between 1 and 100")
}

fn is_retryable(err: &atrium_api::xrpc::Error<get_follows::Error>) -> bool {
    match err {
        // 429 = rate limited; 5xx = transient server issue.
        atrium_api::xrpc::Error::XrpcResponse(resp) => {
            resp.status.as_u16() == 429 || resp.status.is_server_error()
        }
        // Network-level failures (timeouts, connection resets) are worth retrying.
        atrium_api::xrpc::Error::HttpClient(_) => true,
        _ => false,
    }
}

/// Calls `getFollows` once, retrying with exponential backoff on 429/5xx.
async fn get_follows_with_retry(
    agent: &PublicAgent,
    limiter: &RateLimiter,
    params: get_follows::Parameters,
) -> Result<get_follows::Output, String> {
    const MAX_ATTEMPTS: u32 = 8;

    let mut attempt = 0u32;
    loop {
        limiter.wait().await;

        match agent.api.app.bsky.graph.get_follows(params.clone()).await {
            Ok(output) => return Ok(output),
            Err(err) if is_retryable(&err) && attempt < MAX_ATTEMPTS => {
                let backoff = Duration::from_millis(500 * (1u64 << attempt));
                eprintln!(
                    "  transient error ({}), retrying in {:?} (attempt {}/{MAX_ATTEMPTS})",
                    err,
                    backoff,
                    attempt + 1
                );
                tokio::time::sleep(backoff).await;
                attempt += 1;
            }
            Err(err) => return Err(err.to_string()),
        }
    }
}

/// Enumerates every account `actor_did` follows, following pagination cursors.
///
/// `max_pages` caps the depth so very large following lists (celebrities) don't
/// stall the whole run. At 100 follows/page, 10 pages = 1,000 followees.
pub async fn fetch_following(
    agent: &PublicAgent,
    limiter: &RateLimiter,
    actor_did: &str,
    max_pages: usize,
) -> Result<FollowingPage, String> {
    let actor: AtIdentifier = actor_did
        .parse()
        .map_err(|_| format!("invalid AT identifier: {actor_did}"))?;

    let mut cursor: Option<String> = None;
    let mut followees: Vec<ActorRef> = Vec::new();
    let mut subject: Option<ActorRef> = None;
    let mut pages = 0usize;

    loop {
        let params = get_follows::ParametersData {
            actor: actor.clone(),
            cursor: cursor.clone(),
            limit: Some(page_limit()),
        }
        .into();

        let output = get_follows_with_retry(agent, limiter, params).await?;
        let data = output.data;

        if subject.is_none() {
            subject = Some(ActorRef {
                did: data.subject.did.as_str().to_string(),
                handle: data.subject.handle.as_str().to_string(),
            });
        }

        if data.follows.is_empty() {
            break;
        }

        followees.extend(data.follows.iter().map(|profile| ActorRef {
            did: profile.did.as_str().to_string(),
            handle: profile.handle.as_str().to_string(),
        }));

        pages += 1;
        match data.cursor {
            Some(next) if !next.is_empty() && pages < max_pages => cursor = Some(next),
            _ => break,
        }
    }

    Ok(FollowingPage {
        subject: subject.unwrap_or_else(|| ActorRef {
            did: actor_did.to_string(),
            handle: String::new(),
        }),
        followees,
    })
}

/// Creates a client for the public AppView (no login required for public data).
pub fn create_public_agent() -> PublicAgent {
    AtpAgent::new(
        ReqwestClient::new(PUBLIC_ENDPOINT),
        MemorySessionStore::default(),
    )
}

/// Creates an authenticated agent via the user's PDS. Kept for future endpoints
/// that require auth (e.g. writing records or viewing private state).
#[allow(dead_code)]
pub async fn create_agent() -> Option<AtpAgent<MemorySessionStore, ReqwestClient>> {
    use std::env;

    dotenvy::dotenv().ok();
    let username = env::var("BSKY_USERNAME").ok()?;
    let password = env::var("BSKY_PASSWORD").ok()?;

    let agent = AtpAgent::new(
        ReqwestClient::new("https://bsky.social"),
        MemorySessionStore::default(),
    );

    agent.login(&username, &password).await.ok()?;

    Some(agent)
}

/// Reads DID -> handle pairs from `handles.jsonl`, ignoring empty handles.
pub fn load_handles() -> HashMap<String, String> {
    let mut handles = HashMap::new();
    if let Ok(file) = File::open(HANDLES_PATH) {
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let (Some(did), Some(handle)) = (value["did"].as_str(), value["handle"].as_str()) {
                if !handle.is_empty() {
                    handles
                        .entry(did.to_string())
                        .or_insert_with(|| handle.to_string());
                }
            }
        }
    }
    handles
}

/// Returns the handle for `did`, if one is known.
pub fn handle_for<'a>(did: &str, handles: &'a HashMap<String, String>) -> Option<&'a str> {
    handles
        .get(did)
        .map(String::as_str)
        .filter(|handle| !handle.is_empty())
}

/// Human-readable name for an account: its handle, or the DID as a fallback.
pub fn display_name(did: &str, handles: &HashMap<String, String>) -> String {
    handle_for(did, handles)
        .map(str::to_owned)
        .unwrap_or_else(|| did.to_string())
}

/// Reads the set of actor DIDs whose following lists are already stored.
pub fn load_done() -> HashSet<String> {
    let mut done = HashSet::new();
    if let Ok(file) = File::open(DONE_PATH) {
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                done.insert(trimmed.to_string());
            }
        }
    }
    done
}

fn open_append(path: &str) -> std::io::Result<BufWriter<File>> {
    Ok(BufWriter::new(
        OpenOptions::new().create(true).append(true).open(path)?,
    ))
}

/// Fetches following lists for every pending seed DID and appends directed edges
/// (`follower` -> `followee`) to `follows.jsonl`.
///
/// Progress is recorded in `follows_done.txt` so an interrupted run can be
/// resumed by simply running the binary again.
pub async fn fetch_all_follows(
    pending: &[String],
    concurrency: usize,
    max_pages: usize,
    requests_per_sec: f64,
) -> std::io::Result<()> {
    let agent = Arc::new(create_public_agent());
    let limiter = Arc::new(RateLimiter::new(requests_per_sec));

    let mut edges_out = open_append(FOLLOWS_PATH)?;
    let mut handles_out = open_append(HANDLES_PATH)?;
    let mut done_out = open_append(DONE_PATH)?;
    let mut failed_out = open_append(FAILED_PATH)?;

    let mut jobs = futures_util::stream::iter(pending.iter().cloned().map(|did| {
        let agent = Arc::clone(&agent);
        let limiter = Arc::clone(&limiter);
        async move {
            let result = fetch_following(&agent, &limiter, &did, max_pages).await;
            (did, result)
        }
    }))
    .buffer_unordered(concurrency);

    let total = pending.len();
    let (mut ok, mut failed) = (0usize, 0usize);

    while let Some((did, result)) = jobs.next().await {
        match result {
            Ok(FollowingPage { subject, followees }) => {
                // Record the actor's own handle even if they follow nobody.
                writeln!(
                    handles_out,
                    "{}",
                    json!({ "did": &subject.did, "handle": &subject.handle })
                )?;

                for followee in &followees {
                    // DIRECTION: follower (`did`) -> followee.
                    writeln!(
                        edges_out,
                        "{}",
                        json!({
                            "follower": &did,
                            "follower_handle": &subject.handle,
                            "followee": &followee.did,
                            "followee_handle": &followee.handle,
                        })
                    )?;
                }

                writeln!(done_out, "{did}")?;
                ok += 1;
                if ok % 50 == 0 {
                    edges_out.flush()?;
                    handles_out.flush()?;
                    done_out.flush()?;
                    eprintln!("  fetched {ok}/{total} accounts...");
                }
            }
            Err(err) => {
                failed += 1;
                writeln!(failed_out, "{}\t{}", did, err)?;
                if failed <= 10 {
                    eprintln!("  failed {did}: {err}");
                }
            }
        }
    }

    edges_out.flush()?;
    handles_out.flush()?;
    done_out.flush()?;
    failed_out.flush()?;

    println!("Follow fetch complete: {ok} ok, {failed} failed");
    Ok(())
}
