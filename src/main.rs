#![windows_subsystem = "windows"]
use speakv::app::SpeakVApp;
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
