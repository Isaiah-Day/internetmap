use futures_util::StreamExt;
use serde_json::Value;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::Duration;
use tokio::time::{timeout_at, Instant};
use tokio_tungstenite::connect_async;

const URL: &str =
    "wss://jetstream2.us-east.bsky.network/subscribe?wantedCollections=app.bsky.feed.post";

#[tokio::main]
pub async fn collect(run_for: Duration) -> Result<(), Box<dyn std::error::Error>> {
    let (mut ws, _) = connect_async(URL).await?;
    let mut out = BufWriter::new(File::create("posts.jsonl")?);

    // Start the clock once connected so the full 10 seconds is spent collecting.
    let deadline = Instant::now() + run_for;
    let mut events = 0usize;
    let mut new_posts = 0usize;

    loop {
        let msg = match timeout_at(deadline, ws.next()).await {
            Err(_) => break,      // 10 seconds elapsed
            Ok(None) => break,    // server closed the stream
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
        }
    }

    out.flush()?;
    println!(
        "Collected {new_posts} new posts ({events} events total) in {}s -> posts.jsonl",
        run_for.as_secs()
    );
    Ok(())
}