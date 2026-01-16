# SpeakV (Rust Edition)

High-performance, low-latency voice chat application written in Rust.

## Features
- **Low Latency Audio**: Uses `cpal` for direct interaction with audio drivers (WASAPI on Windows).
- **High Performance GUI**: Uses `egui` (via `eframe`) for GPU-accelerated immediate mode GUI.
- **Async Networking**: Uses `tokio` for efficient UDP packet handling.

## Prerequisites
- Rust Toolchain (automatically installed via winget if you followed the assistant).
- C++ Build Tools (Visual Studio Build Tools) - usually required for linking on Windows.

## How to Run
1. **Restart your terminal/IDE** to ensure `cargo` is in your PATH.
2. Run the application:
   ```powershell
   cargo run --release
   ```
   (The first build will take a few minutes to compile dependencies).

## Project Structure
- `src/main.rs`: Entry point.
- `src/app.rs`: GUI logic.
- `src/audio.rs`: Audio capture and playback.
- `src/network.rs`: UDP networking.
