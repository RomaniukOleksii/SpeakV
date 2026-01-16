#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

mod app;
mod audio;
mod network;
mod server;
mod updater;

use app::SpeakVApp;
use eframe::egui;

#[tokio::main]
async fn main() -> eframe::Result<()> {
    // Log to stdout (if you run with `RUST_LOG=debug`).
    
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_title("SpeakV - Rust Low Latency"),
        ..Default::default()
    };
    
    eframe::run_native(
        "SpeakV",
        options,
        Box::new(|cc| Ok(Box::new(SpeakVApp::new(cc)))),
    )
}
