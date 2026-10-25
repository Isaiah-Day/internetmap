use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};

use serde_json::{Value, json};

use crate::bsky::{FOLLOWS_PATH, load_handles};

pub const GRAPH_PATH: &str = "graph.json";
pub const GRAPH_SEEDS_PATH: &str = "graph_seeds.json";
pub const GRAPH_SEEDS_FILTERED_PATH: &str = "graph_seeds_min_followers.json";

pub const DEFAULT_MIN_FOLLOWERS: usize = 10;
const MIN_SEED_COMMUNITY_SIZE: usize = 20;

const EDGE_MEANING: &str = "from = follower, to = followee (from follows to)";

fn node_json(
    did: &str,
    handles: &HashMap<String, String>,
    followers: Option<usize>,
    community: Option<usize>,
) -> Value {
    let mut node = serde_json::Map::new();
    node.insert("id".to_owned(), Value::String(did.to_owned()));
    if let Some(handle) = handles.get(did).filter(|handle| !handle.is_empty()) {
        node.insert("handle".to_owned(), Value::String(handle.clone()));
    }
    if let Some(followers) = followers {
        node.insert("followers".to_owned(), json!(followers));
    }
    if let Some(community) = community {
        node.insert("community".to_owned(), json!(community));
    }
    Value::Object(node)
}

/// Louvain-style local moving on the undirected follow graph. This is only the
/// partition phase; it is enough to turn a hairball into neighbourhoods and is
/// much cheaper than full modularity aggregation on million-node graphs.
pub(crate) fn detect_communities(adj: &[Vec<(usize, f64)>]) -> Vec<usize> {
    const EPS: f64 = 1e-12;

    let n = adj.len();
    if n == 0 {
        return Vec::new();
    }

    let degree: Vec<f64> = adj
        .iter()
        .map(|neighbours| neighbours.iter().map(|(_, w)| *w).sum())
        .collect();
    let total_weight: f64 = degree.iter().sum();
    if total_weight <= 0.0 {
        return (0..n).collect();
    }

    let mut community: Vec<usize> = (0..n).collect();
    let mut community_degree = degree.clone();
    let mut weights: HashMap<usize, f64> = HashMap::new();

    let mut order: Vec<usize> = (0..n).collect();
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;

    for pass in 0..10 {
        // shuffle so layouts are reproducible between runs
        for i in (1..n).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let j = (seed as usize) % (i + 1);
            order.swap(i, j);
        }

        let mut moved = 0usize;
        for &node in &order {
            let old = community[node];
            community_degree[old] -= degree[node];

            weights.clear();
            for &(neighbour, weight) in &adj[node] {
                *weights.entry(community[neighbour]).or_default() += weight;
            }

            let mut best = old;
            let mut best_gain = weights.get(&old).copied().unwrap_or(0.0)
                - degree[node] * community_degree[old] / total_weight;
            for (&candidate, &weight) in &weights {
                let gain = weight - degree[node] * community_degree[candidate] / total_weight;
                let better =
                    gain > best_gain + EPS || ((gain - best_gain).abs() <= EPS && candidate < best);
                if better {
                    best_gain = gain;
                    best = candidate;
                }
            }

            community[node] = best;
            community_degree[best] += degree[node];
            if best != old {
                moved += 1;
            }
        }

        eprintln!("  community pass {}: {moved} node moves", pass + 1);
        if moved == 0 {
            break;
        }
    }

    community
}

fn save_json(path: &str, value: &Value) -> std::io::Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.flush()
}

// Builds the directed graphs from the `follows.jsonl` edges:
// `graph.json`: every observed node and follow edge, minus accounts below `min_followers`
//               (measured as in-degree over the full sampled graph).
// `graph_seeds.json`: the subgraph induced by the seed DIDs (both endpoints must be seeds)
// `graph_seeds_min_followers.json`: `graph_seeds.json` with accounts below `min_followers` removed (and their incident edges).
// An edge `{ "from": A, "to": B }` means **A follows B**.
pub fn build_graphs(seeds: &[String], min_followers: usize) -> std::io::Result<()> {
    // Load known handles (from actor subjects and followee profiles).
    let mut handles = load_handles();

    // collect nodes, handles, and edges
    let seed_set: HashSet<&str> = seeds.iter().map(String::as_str).collect();
    let mut all_nodes: BTreeSet<String> = seeds.iter().cloned().collect();
    all_nodes.extend(handles.keys().cloned());

    let mut seed_edges: Vec<(String, String)> = Vec::new();
    let mut indegree: HashMap<String, usize> = HashMap::new();

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
            *indegree.entry(followee.to_string()).or_default() += 1;

            if seed_set.contains(follower) && seed_set.contains(followee) {
                seed_edges.push((follower.to_string(), followee.to_string()));
            }
        }
    }

    // Default graph: keep only accounts with at least `min_followers` sampled
    // followers (in-degree over all observed edges).
    let kept_nodes: BTreeSet<&str> = all_nodes
        .iter()
        .map(String::as_str)
        .filter(|did| indegree.get(*did).copied().unwrap_or(0) >= min_followers)
        .collect();

    // Detect communities on the *whole* collected follow graph, not just the
    // seed-induced subgraph. Followees followed by a single seed are leaves and
    // cannot change the partition, so we drop them; everything else (all seeds
    // plus shared followees) participates.
    let community_nodes: Vec<&str> = all_nodes
        .iter()
        .map(String::as_str)
        .filter(|did| seed_set.contains(*did) || indegree.get(*did).copied().unwrap_or(0) >= 2)
        .collect();
    let community_index: HashMap<&str, usize> = community_nodes
        .iter()
        .enumerate()
        .map(|(i, did)| (*did, i))
        .collect();

    let mut community_adjacency: Vec<Vec<(usize, f64)>> = vec![Vec::new(); community_nodes.len()];
    if let Ok(file) = File::open(FOLLOWS_PATH) {
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let (Some(from), Some(to)) = (value["follower"].as_str(), value["followee"].as_str())
            else {
                continue;
            };
            let (Some(&a), Some(&b)) = (community_index.get(from), community_index.get(to)) else {
                continue;
            };
            if a != b {
                community_adjacency[a].push((b, 1.0));
                community_adjacency[b].push((a, 1.0));
            }
        }
    }
    let communities = detect_communities(&community_adjacency);
    let community_by_did: HashMap<&str, usize> = community_nodes
        .iter()
        .copied()
        .zip(communities.iter().copied())
        .collect();
    let seed_community_by_did = merge_small_seed_communities(seeds, &seed_edges, &community_by_did);
    println!(
        "Detected {} communities over {} accounts from the full follow graph; {} after merging small seed communities",
        communities.iter().collect::<HashSet<_>>().len(),
        community_nodes.len(),
        seed_community_by_did.values().collect::<HashSet<_>>().len()
    );

    /// Fold seed-visible communities smaller than `MIN_SEED_COMMUNITY_SIZE` into
    /// their best-connected neighbour. Communities with no seed-to-seed links are
    /// collected into one shared "unconnected" bucket so they don't become hundreds
    /// of one-account islands.
    fn merge_small_seed_communities(
        seeds: &[String],
        seed_edges: &[(String, String)],
        community_by_did: &HashMap<&str, usize>,
    ) -> HashMap<String, usize> {
        let mut members: HashMap<usize, Vec<String>> = HashMap::new();
        for did in seeds {
            let community = community_by_did
                .get(did.as_str())
                .copied()
                .unwrap_or_default();
            members.entry(community).or_default().push(did.clone());
        }

        // Community-level seed graph: edge weight = number of seed follows between
        // the two communities.
        let mut adjacency: HashMap<usize, HashMap<usize, usize>> = HashMap::new();
        for (from, to) in seed_edges {
            let a = community_by_did
                .get(from.as_str())
                .copied()
                .unwrap_or_default();
            let b = community_by_did
                .get(to.as_str())
                .copied()
                .unwrap_or_default();
            if a != b {
                *adjacency.entry(a).or_default().entry(b).or_default() += 1;
                *adjacency.entry(b).or_default().entry(a).or_default() += 1;
            }
        }

        loop {
            let mut small: Vec<usize> = members
                .iter()
                .filter(|(_, dids)| dids.len() < MIN_SEED_COMMUNITY_SIZE)
                .map(|(&community, _)| community)
                .collect();
            if small.is_empty() {
                break;
            }
            small.sort_by_key(|community| {
                (
                    members.get(community).map(Vec::len).unwrap_or(0),
                    *community,
                )
            });

            let mut merged_any = false;
            for community in small {
                if !members.contains_key(&community)
                    || members[&community].len() >= MIN_SEED_COMMUNITY_SIZE
                {
                    continue;
                }

                // Prefer the neighbour with the most seed edges; break ties toward
                // larger communities, then lower IDs for reproducibility.
                let best = adjacency.get(&community).and_then(|neighbours| {
                    neighbours
                        .iter()
                        .filter(|(neighbour, _)| {
                            **neighbour != community && members.contains_key(*neighbour)
                        })
                        .max_by(|(a, weight_a), (b, weight_b)| {
                            weight_a
                                .cmp(weight_b)
                                .then_with(|| members[*a].len().cmp(&members[*b].len()))
                                .then_with(|| b.cmp(a))
                        })
                        .map(|(&neighbour, _)| neighbour)
                });
                let Some(best) = best else {
                    continue;
                };

                let moving = members.remove(&community).unwrap_or_default();
                members.entry(best).or_default().extend(moving);

                let old_neighbours = adjacency.remove(&community).unwrap_or_default();
                for (neighbour, weight) in old_neighbours {
                    if neighbour == community {
                        continue;
                    }
                    if let Some(neighbours) = adjacency.get_mut(&neighbour) {
                        neighbours.remove(&community);
                    }
                    if neighbour != best && members.contains_key(&neighbour) {
                        *adjacency
                            .entry(best)
                            .or_default()
                            .entry(neighbour)
                            .or_default() += weight;
                        *adjacency
                            .entry(neighbour)
                            .or_default()
                            .entry(best)
                            .or_default() += weight;
                    }
                }
                if let Some(neighbours) = adjacency.get_mut(&best) {
                    neighbours.remove(&best);
                    neighbours.remove(&community);
                }
                merged_any = true;
            }

            if !merged_any {
                break;
            }
        }

        // Anything still small has no seed-to-seed links. Put them in one bucket.
        let remaining_small: Vec<usize> = members
            .iter()
            .filter(|(_, dids)| dids.len() < MIN_SEED_COMMUNITY_SIZE)
            .map(|(&community, _)| community)
            .collect();
        if !remaining_small.is_empty() {
            let misc = members.keys().copied().max().unwrap_or(0) + 1;
            let mut misc_members = Vec::new();
            for community in remaining_small {
                if let Some(dids) = members.remove(&community) {
                    misc_members.extend(dids);
                }
            }
            members.insert(misc, misc_members);
        }

        members
            .into_iter()
            .flat_map(|(community, dids)| dids.into_iter().map(move |did| (did, community)))
            .collect()
    }

    // Count the edges that survive the node filter (needed for the header).
    let mut kept_edge_count = 0usize;
    if let Ok(file) = File::open(FOLLOWS_PATH) {
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let (Some(from), Some(to)) = (value["follower"].as_str(), value["followee"].as_str())
            else {
                continue;
            };
            if kept_nodes.contains(from) && kept_nodes.contains(to) {
                kept_edge_count += 1;
            }
        }
    }

    // stream the full graph to disk (JSON)
    {
        let mut writer = BufWriter::new(File::create(GRAPH_PATH)?);
        write!(
            writer,
            "{{\"directed\":true,\"edge_meaning\":\"{EDGE_MEANING}\",\"min_followers\":{},\"follower_count_scope\":\"in-degree within the full sampled graph\",\"node_count\":{},\"edge_count\":{},\"nodes\":[",
            min_followers,
            kept_nodes.len(),
            kept_edge_count
        )?;

        for (i, did) in kept_nodes.iter().enumerate() {
            if i > 0 {
                writer.write_all(b",")?;
            }
            serde_json::to_writer(
                &mut writer,
                &node_json(
                    did,
                    &handles,
                    Some(indegree.get(*did).copied().unwrap_or(0)),
                    community_by_did.get(*did).copied(),
                ),
            )?;
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
                if !kept_nodes.contains(from) || !kept_nodes.contains(to) {
                    continue;
                }
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
        "nodes": seeds.iter().map(|did| node_json(
            did,
            &handles,
            Some(indegree.get(did).copied().unwrap_or(0)),
            seed_community_by_did.get(did).copied(),
        )).collect::<Vec<_>>(),
        "edges": seed_edges.iter().map(|(from, to)| json!({ "from": from, "to": to })).collect::<Vec<_>>(),
    });
    save_json(GRAPH_SEEDS_PATH, &seed_graph)?;

    // Filtered version: drop accounts with fewer than `min_followers` sampled
    // followers. "Followers" is in-degree within the seed graph, i.e. how many
    // of our sampled accounts follow this one.
    let mut seed_indegree: HashMap<&str, usize> = HashMap::new();
    for (_, followee) in &seed_edges {
        *seed_indegree.entry(followee.as_str()).or_default() += 1;
    }

    let kept: HashSet<&str> = seeds
        .iter()
        .map(String::as_str)
        .filter(|did| seed_indegree.get(did).copied().unwrap_or(0) >= min_followers)
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
        "nodes": seeds.iter().filter(|did| kept.contains(did.as_str())).map(|did| node_json(
            did,
            &handles,
            Some(indegree.get(did).copied().unwrap_or(0)),
            seed_community_by_did.get(did).copied(),
        )).collect::<Vec<_>>(),
        "edges": filtered_edges.iter().map(|(from, to)| json!({ "from": from, "to": to })).collect::<Vec<_>>(),
    });
    save_json(GRAPH_SEEDS_FILTERED_PATH, &filtered_graph)?;

    Ok(())
}

/// Prints node/edge stats for each built graph. Reads the files from disk so
/// this works whether or not the graphs were rebuilt this run.
pub fn print_stats() {
    println!("\nGraph stats:");
    for path in [GRAPH_PATH, GRAPH_SEEDS_PATH, GRAPH_SEEDS_FILTERED_PATH] {
        let value = match File::open(path)
            .ok()
            .and_then(|file| serde_json::from_reader::<_, Value>(BufReader::new(file)).ok())
        {
            Some(value) => value,
            None => {
                println!("  {path:<32} (not built)");
                continue;
            }
        };

        let nodes = value["node_count"]
            .as_u64()
            .or_else(|| value["nodes"].as_array().map(|a| a.len() as u64))
            .unwrap_or(0);
        let edges = value["edge_count"]
            .as_u64()
            .or_else(|| value["edges"].as_array().map(|a| a.len() as u64))
            .unwrap_or(0);
        let avg_degree = if nodes > 0 {
            edges as f64 / nodes as f64
        } else {
            0.0
        };

        let mut detail = String::new();
        if let Some(min_followers) = value["min_followers"].as_u64() {
            detail.push_str(&format!(", min_followers {min_followers}"));
            if let Some(scope) = value["follower_count_scope"].as_str() {
                detail.push_str(&format!(" ({scope})"));
            }
        }

        println!(
            "  {path:<32} {nodes} nodes, {edges} edges, avg out-degree {avg_degree:.2}{detail}"
        );
    }
}
