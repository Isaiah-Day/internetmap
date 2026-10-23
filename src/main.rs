mod collect;
mod bsky;
use eframe::{run_native, App, CreationContext, NativeOptions};
use egui_graphs::{DefaultGraphView, Graph};


use std::time::Duration;
use std::fs::File;
use std::io::{BufRead, BufReader};

use serde_json::Value;

// pub struct BasicApp {
//     g: egui_graphs::Graph,
// }

// impl BasicApp {
//     fn new(_: &eframe::CreationContext<'_>) -> Self {
//         Self { g:generate_graph() }
//     }
// }

// fn generate_graph() -> Graph {
//     let mut g = Graph::new();

//     let a = g.add_node(());
//     let b = g.add_node(());
//     let c = g.add_node(());

//     g.add_edge(a, b, ());
//     g.add_edge(b, c, ());
//     g.add_edge(c, a, ());

//     g
// }
// impl eframe::App for BasicApp {
//     fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
//         egui::CentralPanel::default().show(ui, |ui| {
//             egui_graphs::DefaultGraphView::new().show(ui, &mut self.g);
//         });
//     }
// }

fn read_jsonl(path: &str) -> std::io::Result<Vec<Value>> {
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
                eprintln!("Skipping invalid JSON at line {}: {}", line_num + 1, e);
            }
        }
    }

    Ok(records)
}

fn extract_dids(records: &[Value]) -> Vec<String> {
    records
        .iter()
        .filter_map(|r| r["did"].as_str().map(String::from))
        .collect()
}

fn save_dids(path: &str, dids: &[String]) -> std::io::Result<()> {
    let file = File::create(path)?;
    serde_json::to_writer_pretty(file, dids)?;
    Ok(())
}

fn main() -> std::io::Result<()>{
    // collect::collect(Duration::from_secs(10));
    // eframe::run_native(
    //     "egui_graphs_basic_demo",
    //     eframe::NativeOptions::default(),
    //     Box::new(|cc| Ok(Box::new(BasicApp::new(cc)))),
    // )
    // .unwrap();
    
    let records: Vec<Value> = read_jsonl("posts.jsonl")?;
    println!("Loaded {} records", records.len());
    let dids = extract_dids(&records);
    save_dids("dids.json", &dids)?;
    println!("Saved {} DIDs to dids.json", dids.len());
    Ok(())
}
