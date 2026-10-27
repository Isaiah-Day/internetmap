use egui::{Pos2, Vec2};
use std::collections::HashMap;

const COMMUNITY_ITERATIONS: usize = 70;
const COMMUNITY_DAMPING: f32 = 0.85;
const COMMUNITY_MAX_STEP: f32 = 4.0;
const COMMUNITY_CENTER_PULL: f32 = 0.000000001;

pub fn force_direct_inside_communities(
    community: &[usize],
    adjacency: &[Vec<(usize, f64)>],
    positions: &mut [Pos2],
) {
    let mut by_community: HashMap<usize, Vec<usize>> = HashMap::new();

    for (node, &c) in community.iter().enumerate() {
        if adjacency[node].is_empty() {
            continue;
        }
        by_community.entry(c).or_default().push(node);
    }
    for (_, nodes) in by_community {
        if nodes.len() < 2 {
            continue;
        }
        force_direct_inside_community(&nodes, adjacency, positions);
    }
}

fn force_direct_inside_community(
    nodes: &[usize],
    adjacency: &[Vec<(usize, f64)>],
    positions: &mut [Pos2],
) {
    // a lot of this stuff is local so that this island does not move relative to others
    let center = nodes
        .iter()
        .fold(Vec2::ZERO, |acc, &node| acc + positions[node].to_vec2())
        / nodes.len() as f32;

    let mut local: Vec<Pos2> = nodes
        .iter()
        .map(|&node| (positions[node].to_vec2() - center).to_pos2())
        .collect();

    let local_index: HashMap<usize, usize> = nodes
        .iter()
        .enumerate()
        .map(|(local_i, &node)| (node, local_i))
        .collect();

    // Resolve intra-community edges to local indices once instead of doing a
    // HashMap lookup and cross-community check for every edge on every pass.
    let local_edges: Vec<Vec<(usize, f32)>> = nodes
        .iter()
        .map(|&node| {
            adjacency[node]
                .iter()
                .filter_map(|&(other, weight)| {
                    local_index
                        .get(&other)
                        .map(|&local_other| (local_other, weight as f32))
                })
                .collect()
        })
        .collect();

    let n = nodes.len() as f32;
    let radius = local
        .iter()
        .map(|p| p.to_vec2().length())
        .fold(0.0, f32::max)
        .max(10.0);
    let area = std::f32::consts::PI * radius * radius;
    let k = (area / n).sqrt(); // ideal edge length                                                                                                                                                  

    let mut disp = vec![Vec2::ZERO; nodes.len()];

    for _ in 0..COMMUNITY_ITERATIONS {
        disp.iter_mut().for_each(|d| *d = Vec2::ZERO);

        // Repulsion. O(n^2) here for clarity.
        // For the giant communities, replace this with a grid/quadtree/Barnes-Hut.
        for a in 0..nodes.len() {
            for b in (a + 1)..nodes.len() {
                let delta = local[a] - local[b];
                let dist = delta.length().max(0.01);
                let force = (k * k) / dist;
                let dir = delta / dist;

                disp[a] += dir * force;
                disp[b] -= dir * force;
            }
        }

        // Attraction along the precomputed intra-community edges.
        for (a, neighbours) in local_edges.iter().enumerate() {
            for &(b, weight) in neighbours {
                let delta = local[b] - local[a];
                let dist = delta.length().max(0.01);
                let force = (dist * dist / k) * weight;
                disp[a] += (delta / dist) * force;
            }

            // Small pull toward the island center to stop sparse fringes escaping.
            disp[a] -= local[a].to_vec2() * COMMUNITY_CENTER_PULL;
        }

        // Damping and maximum step per frame/iteration.
        for (i, d) in disp.iter_mut().enumerate() {
            *d *= COMMUNITY_DAMPING;

            let len = d.length();
            if len > COMMUNITY_MAX_STEP {
                *d = d.normalized() * COMMUNITY_MAX_STEP;
            }

            local[i] += *d;
        }
    }

    // Move the settled local layout back around the original island center.
    for (local_i, &node) in nodes.iter().enumerate() {
        positions[node] = (local[local_i].to_vec2() + center).to_pos2();
    }
}
