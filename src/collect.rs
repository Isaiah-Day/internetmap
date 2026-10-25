use futures_util::StreamExt;
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::time::Duration;
use tokio::time::{Instant, timeout_at};
use tokio_tungstenite::connect_async;

pub const POSTS_PATH: &str = "posts.jsonl";
pub const DIDS_PATH: &str = "dids.json";

const URL: &str =
    "wss://jetstream2.us-east.bsky.network/subscribe?wantedCollections=app.bsky.feed.post";

// scrape new bsky posts
pub async fn collect(run_for: Duration) -> Result<(), Box<dyn std::error::Error>> {
    let (mut ws, _) = connect_async(URL).await?;
    let mut out = BufWriter::new(File::create("posts.jsonl")?);

    // Start the clock once connected so the full duration is spent collecting.
    let deadline = Instant::now() + run_for;
    let mut events = 0usize;
    let mut new_posts = 0usize;

    loop {
        let msg = match timeout_at(deadline, ws.next()).await {
            Err(_) => break,   // run_for elapsed
            Ok(None) => break, // server closed the stream
            Ok(Some(m)) => m?,
        };
        if !msg.is_text() {
            continue;
        }
        let text = msg.to_text()?;
        events += 1;

        let v: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Keep only newly created posts (skip updates and deletes).
        if v["kind"] == "commit" && v["commit"]["operation"] == "create" {
            new_posts += 1;
            writeln!(out, "{}", text)?; // one raw JSON event per line

            // Flush periodically so an interrupted run doesn't lose everything.
            if new_posts % 100 == 0 {
                out.flush()?;
                eprintln!("  collected {new_posts} new posts...");
            }
        }
    }

    out.flush()?;
    println!(
        "Collected {new_posts} new posts ({events} events total) in {}s -> {POSTS_PATH}",
        run_for.as_secs()
    );
    Ok(())
}

/// Reads a JSONL file, skipping blank lines and warning about malformed ones.
pub fn read_jsonl(path: &str) -> std::io::Result<Vec<Value>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for (line_num, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue; // Skip empty lines
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => records.push(value),
            Err(e) => {
                eprintln!("Skipping invalid JSON at line {}: {e}", line_num + 1);
            }
        }
    }

    Ok(records)
}

/// Pulls the `did` field out of every Jetstream event record.
pub fn extract_dids(records: &[Value]) -> Vec<String> {
    records
        .iter()
        .filter_map(|record| record["did"].as_str().map(String::from))
        .collect()
}

pub fn save_dids(path: &str, dids: &[String]) -> std::io::Result<()> {
    let file = File::create(path)?;
    serde_json::to_writer_pretty(file, dids)?;
    Ok(())
}

/// Loads the sorted, deduplicated DIDs saved by a previous collection run.
pub fn load_dids() -> std::io::Result<Vec<String>> {
    let file = File::open(DIDS_PATH)?;
    serde_json::from_reader(BufReader::new(file))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Collects posts for `run_for`, then extracts/saves the unique author DIDs.
pub async fn collect_and_extract(
    run_for: Duration,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    collect(run_for).await?;

    let records = read_jsonl(POSTS_PATH)?;
    println!("Loaded {} records", records.len());

    let mut dids = extract_dids(&records);
    dids.sort();
    dids.dedup();

    save_dids(DIDS_PATH, &dids)?;
    println!("Saved {} unique DIDs to {DIDS_PATH}", dids.len());
    Ok(dids)
}
