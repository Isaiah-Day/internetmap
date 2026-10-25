use std::collections::HashMap;
use std::fs;
use std::path::Path;

use eframe::egui;
use egui::epaint::{CircleShape, TextShape};
use egui::{Color32, FontId, Pos2, Rect, Shape, Stroke, Vec2};
use egui_graphs::{
    DisplayEdge, DisplayNode, DrawContext, EdgeProps, GraphView, Layout, LayoutState, Node,
    NodeProps, SettingsInteraction, SettingsNavigation, SettingsStyle,
};
use petgraph::graph::IndexType;
use petgraph::stable_graph::StableGraph;
use petgraph::{Directed, EdgeType};
use serde_json::Value;

use crate::bsky;
use crate::graph::detect_communities;

#[derive(serde::Deserialize)]
struct RawEdge {
    from: String,
    to: String,
}

#[derive(serde::Deserialize)]
struct RawNode {
    id: String,
    #[serde(default)]
    handle: String,
    #[serde(default)]
    followers: Option<usize>,
    #[serde(default)]
    community: Option<usize>,
}

#[derive(Clone, Debug)]
struct MapNode {
    handle: String,
    followers: usize,
    community: usize,
    major: bool,
    pos: Pos2,
}

#[derive(Clone, Copy, Debug)]
struct MapEdge {
    /// Part of the readable "backbone" shown when nothing is hovered.
    salient: bool,
}

/// Fixed layout: all positions are computed up front by `build`.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct FixedState;

#[derive(Default)]
struct FixedLayout {
    state: FixedState,
}

impl LayoutState for FixedState {}

impl Layout<FixedState> for FixedLayout {
    fn from_state(state: FixedState) -> impl Layout<FixedState> {
        Self { state }
    }

    fn next<N, E, Ty, Ix, Dn, De>(
        &mut self,
        _g: &mut egui_graphs::Graph<N, E, Ty, Ix, Dn, De>,
        _ui: &egui::Ui,
    ) where
        N: Clone,
        E: Clone,
        Ty: EdgeType,
        Ix: IndexType,
        Dn: DisplayNode<N, E, Ty, Ix>,
        De: DisplayEdge<N, E, Ty, Ix, Dn>,
    {
        // Positions come from the map layout; nothing to simulate.
    }

    fn state(&self) -> FixedState {
        self.state.clone()
    }
}

#[derive(Clone, Debug)]
struct MapNodeShape {
    pos: Pos2,
    selected: bool,
    dragged: bool,
    hovered: bool,
    color: Color32,
    label: String,
    radius: f32,
    major: bool,
}

impl From<NodeProps<MapNode>> for MapNodeShape {
    fn from(props: NodeProps<MapNode>) -> Self {
        Self {
            pos: props.location(),
            selected: props.selected,
            dragged: props.dragged,
            hovered: props.hovered,
            color: community_color(props.payload.community),
            label: props.label,
            radius: node_radius(props.payload.followers),
            major: props.payload.major,
        }
    }
}

impl<E: Clone, Ty: EdgeType, Ix: IndexType> DisplayNode<MapNode, E, Ty, Ix> for MapNodeShape {
    fn is_inside(&self, pos: Pos2) -> bool {
        (pos - self.pos).length() <= self.radius
    }

    fn closest_boundary_point(&self, dir: Vec2) -> Pos2 {
        self.pos + dir.normalized() * self.radius
    }

    fn shapes(&mut self, ctx: &DrawContext) -> Vec<Shape> {
        let center = ctx.meta.canvas_to_screen_pos(self.pos);
        let radius = ctx.meta.canvas_to_screen_size(self.radius).max(1.0);
        let stroke = if self.hovered || self.selected {
            Stroke::new(1.5, Color32::WHITE)
        } else {
            Stroke::NONE
        };

        let mut shapes = vec![
            CircleShape {
                center,
                radius,
                fill: self.color,
                stroke,
            }
            .into(),
        ];

        // Labels are progressive disclosure: hover/selection always shows one, and
        // the optional "label hubs" toggle shows only the most-followed accounts.
        let show_label = self.hovered
            || self.selected
            || self.dragged
            || (ctx.style.labels_always() && self.major);
        if !show_label || self.label.is_empty() {
            return shapes;
        }

        let text_color = Color32::from_gray(235);
        let galley = ctx.ctx.fonts_mut(|fonts| {
            fonts.layout_no_wrap(self.label.clone(), FontId::proportional(12.0), text_color)
        });
        let label_pos = Pos2::new(
            center.x - galley.size().x / 2.0,
            center.y - radius - galley.size().y - 4.0,
        );
        let bg = Rect::from_min_size(
            label_pos - Vec2::new(3.0, 1.0),
            galley.size() + Vec2::new(6.0, 2.0),
        );
        shapes.push(Shape::rect_filled(bg, 3.0, Color32::from_black_alpha(185)));
        shapes.push(TextShape::new(label_pos, galley, text_color).into());
        shapes
    }

    fn update(&mut self, state: &NodeProps<MapNode>) {
        self.pos = state.location();
        self.selected = state.selected;
        self.dragged = state.dragged;
        self.hovered = state.hovered;
        self.color = community_color(state.payload.community);
        self.label = state.label.clone();
        self.radius = node_radius(state.payload.followers);
        self.major = state.payload.major;
    }
}

#[derive(Clone, Debug)]
struct MapEdgeShape {
    salient: bool,
    selected: bool,
}

impl From<EdgeProps<MapEdge>> for MapEdgeShape {
    fn from(props: EdgeProps<MapEdge>) -> Self {
        Self {
            salient: props.payload.salient,
            selected: props.selected,
        }
    }
}

impl<N: Clone, Ty: EdgeType, Ix: IndexType, Dn: DisplayNode<N, MapEdge, Ty, Ix>>
    DisplayEdge<N, MapEdge, Ty, Ix, Dn> for MapEdgeShape
{
    fn shapes(
        &mut self,
        start: &Node<N, MapEdge, Ty, Ix, Dn>,
        end: &Node<N, MapEdge, Ty, Ix, Dn>,
        ctx: &DrawContext,
    ) -> Vec<Shape> {
        if start.id() == end.id() {
            return Vec::new();
        }

        let active =
            start.hovered() || end.hovered() || start.selected() || end.selected() || self.selected;
        if !self.salient && !active {
            return Vec::new();
        }

        let delta = end.location() - start.location();
        if delta.length_sq() < 1e-6 {
            return Vec::new();
        }
        let dir = delta.normalized();
        let a = start.display().closest_boundary_point(dir);
        let b = end.display().closest_boundary_point(-dir);

        let (color, width) = if active {
            (Color32::from_rgba_unmultiplied(225, 235, 250, 180), 1.25)
        } else {
            (Color32::from_rgba_unmultiplied(155, 170, 195, 30), 0.35)
        };

        vec![Shape::line_segment(
            [
                ctx.meta.canvas_to_screen_pos(a),
                ctx.meta.canvas_to_screen_pos(b),
            ],
            Stroke::new(width, color),
        )]
    }

    fn update(&mut self, state: &EdgeProps<MapEdge>) {
        self.salient = state.payload.salient;
        self.selected = state.selected;
    }

    fn is_inside(
        &self,
        _start: &Node<N, MapEdge, Ty, Ix, Dn>,
        _end: &Node<N, MapEdge, Ty, Ix, Dn>,
        _pos: Pos2,
    ) -> bool {
        false
    }
}

fn node_radius(followers: usize) -> f32 {
    1.4 + 2.6 * (followers as f32 + 1.0).log10()
}

fn community_color(community: usize) -> Color32 {
    // Deterministic, well-separated hues; adjacent communities are likely to differ.
    let hue = (community as f32 * 0.618_034) % 1.0;
    let (r, g, b) = hsv_to_rgb(hue, 0.62, 0.95);
    Color32::from_rgb(r, g, b)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let h = (h.fract() + 1.0).fract() * 6.0;
    let i = h.floor() as i32;
    let f = h - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match i % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    (
        (r.clamp(0.0, 1.0) * 255.0) as u8,
        (g.clamp(0.0, 1.0) * 255.0) as u8,
        (b.clamp(0.0, 1.0) * 255.0) as u8,
    )
}

/// Places communities as separate islands and low-degree singleton accounts in
/// a compact gallery underneath them.
fn layout_map(
    community: &[usize],
    followers: &[usize],
    adjacency: &[Vec<(usize, f64)>],
) -> Vec<Pos2> {
    const GOLDEN_ANGLE: f32 = 2.399_963_2;
    const NODE_SPACING: f32 = 8.0;
    const ISLAND_PADDING: f32 = 55.0;
    const SINGLE_SPACING: f32 = 13.0;

    let n = community.len();
    let mut positions = vec![Pos2::ZERO; n];
    if n == 0 {
        return positions;
    }

    // Accounts with no links in the displayed seed graph go to a compact
    // gallery, regardless of which full-graph community they belong to.
    let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut isolated = Vec::new();
    for (node, &c) in community.iter().enumerate() {
        if adjacency[node].is_empty() {
            isolated.push(node);
        } else {
            members.entry(c).or_default().push(node);
        }
    }

    let mut islands = Vec::new();
    for (c, mut nodes) in members {
        nodes.sort_by(|&a, &b| followers[b].cmp(&followers[a]).then_with(|| a.cmp(&b)));
        islands.push((c, nodes));
    }
    islands.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    isolated.sort_by(|&a, &b| followers[b].cmp(&followers[a]).then_with(|| a.cmp(&b)));

    let max_diameter = islands
        .iter()
        .map(|(_, nodes)| 2.0 * island_radius(nodes.len(), NODE_SPACING))
        .fold(0.0f32, f32::max);
    let row_width = (max_diameter * 3.0).max(1400.0);

    let mut x = 0.0f32;
    let mut y = 0.0f32;
    let mut row_height = 0.0f32;

    for (_, nodes) in &islands {
        let radius = island_radius(nodes.len(), NODE_SPACING);
        let diameter = radius * 2.0;
        if x > 0.0 && x + diameter > row_width {
            x = 0.0;
            y += row_height + ISLAND_PADDING;
            row_height = 0.0;
        }
        let center = Pos2::new(x + radius, y + radius);

        for (i, &node) in nodes.iter().enumerate() {
            let ring = NODE_SPACING * (i as f32).sqrt();
            let angle = i as f32 * GOLDEN_ANGLE;
            positions[node] = center + Vec2::new(ring * angle.cos(), ring * angle.sin());
        }

        x += diameter + ISLAND_PADDING;
        row_height = row_height.max(diameter);
    }

    // Everything below the islands is for accounts with no sampled in-graph links.
    if !isolated.is_empty() {
        let gallery_y = y + row_height + ISLAND_PADDING;
        let columns = ((row_width / SINGLE_SPACING).floor() as usize).max(1);
        for (i, &node) in isolated.iter().enumerate() {
            let col = i % columns;
            let row = i / columns;
            positions[node] = Pos2::new(
                col as f32 * SINGLE_SPACING,
                gallery_y + row as f32 * SINGLE_SPACING,
            );
        }
    }

    // Center the whole map around the origin.
    let mut min = Pos2::new(f32::INFINITY, f32::INFINITY);
    let mut max = Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY);
    for p in &positions {
        min.x = min.x.min(p.x);
        min.y = min.y.min(p.y);
        max.x = max.x.max(p.x);
        max.y = max.y.max(p.y);
    }
    let offset = ((min + max.to_vec2()) / 2.0).to_vec2();
    for p in &mut positions {
        *p -= offset;
    }
    positions
}

fn island_radius(size: usize, node_spacing: f32) -> f32 {
    (node_spacing * (size as f32).sqrt() + 12.0).max(24.0)
}

fn build(
    json_in: &str,
) -> (
    egui_graphs::Graph<
        MapNode,
        MapEdge,
        Directed,
        petgraph::stable_graph::DefaultIx,
        MapNodeShape,
        MapEdgeShape,
    >,
    usize,
    usize,
    usize,
) {
    let value: Value = serde_json::from_str(json_in).expect("invalid graph JSON");
    let edges: Vec<RawEdge> = serde_json::from_value(value["edges"].clone())
        .expect("graph JSON is missing an `edges` array");
    let raw_nodes: Vec<RawNode> =
        serde_json::from_value(value["nodes"].clone()).unwrap_or_default();

    let mut handles = bsky::load_handles();
    for node in &raw_nodes {
        if !node.handle.is_empty() {
            handles.insert(node.id.clone(), node.handle.clone());
        }
    }

    let node_index: HashMap<&str, usize> = raw_nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (node.id.as_str(), i))
        .collect();
    let n = raw_nodes.len();

    let mut followers_count = vec![0usize; n];
    let mut adjacency: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    let mut parsed_edges: Vec<(usize, usize)> = Vec::with_capacity(edges.len());

    for edge in &edges {
        let (Some(&a), Some(&b)) = (
            node_index.get(edge.from.as_str()),
            node_index.get(edge.to.as_str()),
        ) else {
            continue;
        };
        followers_count[b] += 1;
        if a != b {
            adjacency[a].push((b, 1.0));
            adjacency[b].push((a, 1.0));
        }
        parsed_edges.push((a, b));
    }

    // Prefer community IDs computed over the full collected follow graph by
    // `--build-graph`. Older graph files (and the fallback graph.json path) do
    // not have them, so detect on the displayed graph in that case.
    let stored_communities = raw_nodes.iter().all(|node| node.community.is_some());
    let community: Vec<usize> = if stored_communities {
        raw_nodes
            .iter()
            .map(|node| node.community.unwrap_or(0))
            .collect()
    } else {
        detect_communities(&adjacency)
    };

    for (i, node) in raw_nodes.iter().enumerate() {
        if let Some(followers) = node.followers {
            followers_count[i] = followers;
        }
    }
    let positions = layout_map(&community, &followers_count, &adjacency);

    // Only draw a readable backbone by default: each account's top few follows,
    // plus every edge pointing at a well-followed hub. Hovering reveals that
    // account's incident backbone edges in bright white.
    let mut salient = vec![false; parsed_edges.len()];
    let mut edge_indices_by_source: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, &(a, _)) in parsed_edges.iter().enumerate() {
        edge_indices_by_source[a].push(i);
    }
    for edge_indices in &mut edge_indices_by_source {
        edge_indices.sort_by(|&a, &b| {
            followers_count[parsed_edges[b].1]
                .cmp(&followers_count[parsed_edges[a].1])
                .then_with(|| a.cmp(&b))
        });
        for &edge_index in edge_indices.iter().take(3) {
            salient[edge_index] = true;
        }
    }
    for (i, &(_, b)) in parsed_edges.iter().enumerate() {
        if followers_count[b] >= 100 {
            salient[i] = true;
        }
    }

    let mut major_nodes = vec![false; n];
    let mut by_followers: Vec<usize> = (0..n).collect();
    by_followers.sort_by(|&a, &b| {
        followers_count[b]
            .cmp(&followers_count[a])
            .then_with(|| a.cmp(&b))
    });
    for &node in by_followers.iter().take(100) {
        major_nodes[node] = true;
    }

    let mut stable: StableGraph<MapNode, MapEdge, Directed> = StableGraph::new();
    let mut ids = Vec::with_capacity(n);
    for (i, raw) in raw_nodes.iter().enumerate() {
        let handle = if raw.handle.is_empty() {
            handles.get(&raw.id).cloned().unwrap_or_default()
        } else {
            raw.handle.clone()
        };
        let label = if handle.is_empty() {
            short_did(&raw.id)
        } else {
            handle.clone()
        };
        ids.push(stable.add_node(MapNode {
            handle: label,
            followers: followers_count[i],
            community: community[i],
            major: major_nodes[i],
            pos: positions[i],
        }));
    }

    for (i, &(a, b)) in parsed_edges.iter().enumerate() {
        stable.add_edge(
            ids[a],
            ids[b],
            MapEdge {
                salient: salient[i],
            },
        );
    }

    let graph = egui_graphs::to_graph_custom(
        &stable,
        |node: &mut Node<
            MapNode,
            MapEdge,
            Directed,
            petgraph::stable_graph::DefaultIx,
            MapNodeShape,
        >| {
            node.set_label(node.payload().handle.clone());
            node.set_location(node.payload().pos);
        },
        |_edge: &mut egui_graphs::Edge<
            MapNode,
            MapEdge,
            Directed,
            petgraph::stable_graph::DefaultIx,
            MapNodeShape,
            MapEdgeShape,
        >| {},
    );

    let displayed_edges = salient.iter().filter(|&&s| s).count();
    let communities = community
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len();

    (graph, n, displayed_edges, communities)
}

fn short_did(did: &str) -> String {
    did.rsplit(':').next().unwrap_or(did).to_owned()
}

pub fn display() -> Result<(), Box<dyn std::error::Error>> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 860.0]),
        ..Default::default()
    };

    // The seed graph contains every sampled account; graph.json is the
    // thresholded view used by the stats command.
    let path = if Path::new("graph_seeds.json").exists() {
        "graph_seeds.json"
    } else {
        "graph.json"
    };
    let json = fs::read_to_string(path)?;
    let (mut graph, node_count, shown_edges, community_count) = build(&json);

    let navigation = SettingsNavigation::new()
        .with_zoom_and_pan_enabled(true)
        .with_fit_to_screen_enabled(false)
        .with_zoom_speed(0.1);
    let interactions = SettingsInteraction::new()
        .with_dragging_enabled(true)
        .with_hover_enabled(true)
        .with_node_selection_enabled(true);

    let mut label_hubs = false;

    eframe::run_ui_native("internetmap", options, move |ui, _frame| {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("internetmap");
                ui.separator();
                ui.label(format!("{node_count} accounts"));
                ui.label(format!("{community_count} communities"));
                ui.label(format!("{shown_edges} follow edges drawn"));
                ui.separator();
                ui.checkbox(&mut label_hubs, "label top accounts");
                ui.label("dot size = sampled followers · color = community");
                ui.label("hover to highlight, click to pin a label");
            });
            ui.separator();

            let style = SettingsStyle::new().with_labels_always(label_hubs);
            GraphView::<FixedState, FixedLayout>::new()
                .with_navigations(&navigation)
                .with_interactions(&interactions)
                .with_styles(&style)
                .with_id(Some("internetmap-map".to_owned()))
                .show(ui, &mut graph);
        });
    })?;

    Ok(())
}
