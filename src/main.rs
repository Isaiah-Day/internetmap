mod bsky;
mod collect;
mod display;
mod graph;

use std::path::Path;
use std::time::Duration;

const COLLECT_SECS: u64 = 5 * 60;

/// Reads a `--flag value` pair from the command line.
fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // cmd line args
    let args: Vec<String> = std::env::args().skip(1).collect();

    let recollect = args.iter().any(|arg| arg == "--recollect");
    let should_build = args.iter().any(|arg| arg == "--build-graph");
    let limit = arg_value(&args, "--limit").and_then(|n| n.parse::<usize>().ok());
    let max_pages = arg_value(&args, "--max-pages")
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(bsky::MAX_PAGES);
    let concurrency = arg_value(&args, "--concurrency")
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(bsky::CONCURRENCY);
    let requests_per_sec = arg_value(&args, "--rps")
        .and_then(|n| n.parse::<f64>().ok())
        .unwrap_or(bsky::REQUESTS_PER_SEC);
    let min_followers = arg_value(&args, "--min-followers")
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(graph::DEFAULT_MIN_FOLLOWERS);

    // collect and extract dids if not already done so
    if recollect || !Path::new(collect::DIDS_PATH).exists() {
        collect::collect_and_extract(Duration::from_secs(COLLECT_SECS)).await?;
    } else {
        println!(
            "Using existing {} (pass --recollect to fetch fresh posts)",
            collect::DIDS_PATH
        );
    }

    // get DIDs as data
    let all_dids: Vec<String> = collect::load_dids()?;
    let seeds: Vec<String> = match limit {
        Some(n) => all_dids.into_iter().take(n).collect(),
        None => all_dids,
    };

    let done = bsky::load_done(); // how DIDs have been scraped for follow data
    let pending: Vec<String> = seeds
        .iter()
        .filter(|did| !done.contains(*did))
        .cloned()
        .collect();

    println!(
        "Seeds: {} | already fetched: {} | pending: {}",
        seeds.len(),
        seeds.len() - pending.len(),
        pending.len()
    );

    if pending.is_empty() {
        println!("All seeds already fetched; rebuilding graph from existing data.");
    } else {
        println!(
            "Fetching at {requests_per_sec} req/s, {concurrency} concurrent, max {max_pages} pages/account"
        );
        bsky::fetch_all_follows(&pending, concurrency, max_pages, requests_per_sec).await?;
    }

    // create directed graph
    if should_build {
        graph::build_graphs(&seeds, min_followers)?;
    }

    graph::print_stats();

    display::display()?;

    Ok(())
}
