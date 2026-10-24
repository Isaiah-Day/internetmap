use std::collections::HashMap;
use petgraph::{stable_graph::StableGraph, Directed};
use serde_json::Value;
use std::fs;

use eframe::egui;

#[derive(serde::Deserialize)]
struct Edge { from: String, to: String } 

fn build(json_in: &str) -> StableGraph<(), (), Directed> {
    let value: Value = serde_json::from_str(json_in).expect("invalid graph JSON");
    let edges: Vec<Edge> = serde_json::from_value(value["edges"].clone())
        .expect("graph JSON is missing an `edges` array");
    let mut g = StableGraph::new();
    let mut ids = HashMap::new();

    for e in &edges {
        let a = *ids.entry(e.from.clone()).or_insert_with(|| g.add_node(()));
        let b = *ids.entry(e.to.clone()).or_insert_with(|| g.add_node(()));
        g.add_edge(a, b, ());    
    }
    g
}

pub fn display() -> Result<(), Box<dyn std::error::Error>> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([320.0, 240.0]),
        ..Default::default()
    };
    let json = fs::read_to_string("graph_seeds_min_followers.json")?;
    let mut graph: egui_graphs::Graph = egui_graphs::Graph::from(&build(&json));


    let navigation = egui_graphs::SettingsNavigation::new()
        .with_zoom_and_pan_enabled(true)
        .with_fit_to_screen_enabled(false)
        .with_zoom_speed(0.5);
    let style = egui_graphs::SettingsStyle::new()
        .with_edge_stroke_hook(|_selected, _order, stroke, _egui_style| {
            let mut s = stroke;
            s.width=0.001;
            s
        });

    // let mut name = "Arthur".to_owned();
    // let mut age = 42;

    eframe::run_ui_native("My egui App", options, move |ui, _frame| {
        egui::CentralPanel::default().show(ui, |ui| {
            // ui.heading("My egui Application");
            // ui.horizontal(|ui| {
            //     let name_label = ui.label("Your name: ");
            //     ui.text_edit_singleline(&mut name)
            //         .labelled_by(name_label.id);
            // });
            // ui.add(egui::Slider::new(&mut age, 0..=120).text("age"));
            // if ui.button("Increment").clicked() {
            //     age += 1;
            // }
            // ui.label(format!("Hello '{name}', age {age}"));
            egui_graphs::DefaultGraphView::new()
                .with_navigations(&navigation)
                .with_styles(&style)
                .show(ui, &mut graph);
        });
    })?;

    Ok(())
}