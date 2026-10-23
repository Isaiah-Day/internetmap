use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};

use serde_json::{Value, json};

use crate::bsky::{FOLLOWS_PATH, HANDLES_PATH};

pub const GRAPH_PATH: &str = "graph.json";
pub const GRAPH_SEEDS_PATH: &str = "graph_seeds.json";
pub const GRAPH_SEEDS_FILTERED_PATH: &str = "graph_seeds_min_followers.json";


pub const DEFAULT_MIN_FOLLOWERS: usize = 10;

const EDGE_MEANING: &str = "from = follower, to = followee (from follows to)";

fn node_json(did: &str, handles: &HashMap<String, String>) -> Value {
    match handles.get(did) {
        Some(handle) if !handle.is_empty() => json!({ "id": did, "handle": handle }),
        _ => json!({ "id": did }),
    }
}

fn save_json(path: &str, value: &Value) -> std::io::Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.flush()
}

// Builds the directed graphs from the `follows.jsonl` edges:
// `graph.json`: every observed node and follow edge.
// `graph_seeds.json`: the subgraph induced by the seed DIDs (both endpoints must be seeds)
// `graph_seeds_min_followers.json`: `graph_seeds.json` with accounts below `min_followers` removed (and their incident edges).
// An edge `{ "from": A, "to": B }` means **A follows B**.
pub fn build_graphs(seeds: &[String], min_followers: usize) -> std::io::Result<()> {
    // Load known handles (from actor subjects and followee profiles).
    let mut handles: HashMap<String, String> = HashMap::new();
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

    // collect nodes, handles, and edges
    let seed_set: HashSet<&str> = seeds.iter().map(String::as_str).collect();
    let mut all_nodes: BTreeSet<String> = seeds.iter().cloned().collect();
    all_nodes.extend(handles.keys().cloned());

    let mut seed_edges: Vec<(String, String)> = Vec::new();
    let mut edge_count = 0usize;

    if let Ok(file) = File::open(FOLLOWS_PATH) {
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let (Some(follower), Some(followee)) =
                (value["follower"].as_str(), value["followee"].as_str())
            else {
                continue;
            };

            for (did_field, handle_field) in [
                ("follower", "follower_handle"),
                ("followee", "followee_handle"),
            ] {
                if let (Some(did), Some(handle)) =
                    (value[did_field].as_str(), value[handle_field].as_str())
                {
                    if !handle.is_empty() {
                        handles
                            .entry(did.to_string())
                            .or_insert_with(|| handle.to_string());
                    }
                }
            }

            all_nodes.insert(follower.to_string());
            all_nodes.insert(followee.to_string());
            edge_count += 1;

            if seed_set.contains(follower) && seed_set.contains(followee) {
                seed_edges.push((follower.to_string(), followee.to_string()));
            }
        }
    }

    // stream the full graph to disk (JSON)
    {
        let mut writer = BufWriter::new(File::create(GRAPH_PATH)?);
        write!(
            writer,
            "{{\"directed\":true,\"edge_meaning\":\"{EDGE_MEANING}\",\"node_count\":{},\"edge_count\":{},\"nodes\":[",
            all_nodes.len(),
            edge_count
        )?;

        for (i, did) in all_nodes.iter().enumerate() {
            if i > 0 {
                writer.write_all(b",")?;
            }
            serde_json::to_writer(&mut writer, &node_json(did, &handles))?;
        }

        writer.write_all(b"],\"edges\":[")?;
        if let Ok(file) = File::open(FOLLOWS_PATH) {
            let mut first = true;
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let (Some(from), Some(to)) =
                    (value["follower"].as_str(), value["followee"].as_str())
                else {
                    continue;
                };
                if !first {
                    writer.write_all(b",")?;
                }
                first = false;
                serde_json::to_writer(&mut writer, &json!({ "from": from, "to": to }))?;
            }
        }
        writer.write_all(b"]}")?;
        writer.flush()?;
    }

    // Seed-induced subgraph: both endpoints must be seeds.
    let seed_graph = json!({
        "directed": true,
        "edge_meaning": EDGE_MEANING,
        "node_count": seeds.len(),
        "edge_count": seed_edges.len(),
        "nodes": seeds.iter().map(|did| node_json(did, &handles)).collect::<Vec<_>>(),
        "edges": seed_edges.iter().map(|(from, to)| json!({ "from": from, "to": to })).collect::<Vec<_>>(),
    });
    save_json(GRAPH_SEEDS_PATH, &seed_graph)?;

    // Filtered version: drop accounts with fewer than `min_followers` sampled
    // followers. "Followers" is in-degree within the seed graph, i.e. how many
    // of our sampled accounts follow this one.
    let mut indegree: HashMap<&str, usize> = HashMap::new();
    for (_, followee) in &seed_edges {
        *indegree.entry(followee.as_str()).or_default() += 1;
    }

    let kept: HashSet<&str> = seeds
        .iter()
        .map(String::as_str)
        .filter(|did| indegree.get(did).copied().unwrap_or(0) >= min_followers)
        .collect();

    let filtered_edges: Vec<&(String, String)> = seed_edges
        .iter()
        .filter(|(from, to)| kept.contains(from.as_str()) && kept.contains(to.as_str()))
        .collect();

    let filtered_graph = json!({
        "directed": true,
        "edge_meaning": EDGE_MEANING,
        "min_followers": min_followers,
        "follower_count_scope": "in-degree within the sampled seed graph",
        "node_count": kept.len(),
        "edge_count": filtered_edges.len(),
        "nodes": seeds.iter().filter(|did| kept.contains(did.as_str())).map(|did| node_json(did, &handles)).collect::<Vec<_>>(),
        "edges": filtered_edges.iter().map(|(from, to)| json!({ "from": from, "to": to })).collect::<Vec<_>>(),
    });
    save_json(GRAPH_SEEDS_FILTERED_PATH, &filtered_graph)?;

    println!(
        "Saved {} ({} nodes, {} edges), {} ({} nodes, {} edges), and {} ({} nodes, {} edges)",
        GRAPH_PATH,
        all_nodes.len(),
        edge_count,
        GRAPH_SEEDS_PATH,
        seeds.len(),
        seed_edges.len(),
        GRAPH_SEEDS_FILTERED_PATH,
        kept.len(),
        filtered_edges.len(),
    );
    Ok(())
}
