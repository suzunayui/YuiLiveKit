//! Installs the real VOICEVOX engine into a folder, starts it on a spare port, lists its
//! speakers and synthesizes one line, then stops it. Run:
//!   cargo run --example voicevox_probe -- <folder> [port]
//! The folder is created if needed; an engine already installed there is reused.

use yui_live_kit::{
    speech::{self, Voice},
    voicevox::Engine,
};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("folder"));
    let port = args.next().unwrap_or_else(|| "50121".into());
    let base = format!("http://127.0.0.1:{port}");
    let engine = Engine::new(dir.join("engine"), &base, |state| {
        if !state.status.is_empty() {
            println!("[state] installed={} running={} busy={} {}", state.installed, state.running, state.busy, state.status);
        }
    });
    if !engine.installed() {
        let started = std::time::Instant::now();
        engine.install().await.expect("install");
        println!("installed in {:.0}s", started.elapsed().as_secs_f64());
    }
    let started = std::time::Instant::now();
    engine.ensure_running().await.expect("start");
    println!("answering after {:.1}s", started.elapsed().as_secs_f64());
    let client = reqwest::Client::new();
    let speakers = speech::voicevox_speakers(&client, &base).await.expect("speakers");
    println!("speakers: {}", speakers.len());
    let mut uuids: Vec<String> = speakers.iter().map(|s| s.uuid.clone()).collect();
    uuids.dedup();
    let started = std::time::Instant::now();
    let icons = speech::voicevox_icons(&client, &base, &uuids).await;
    println!("icons: {} of {} styles in {:.1}s", icons.len(), speakers.len(), started.elapsed().as_secs_f64());
    let voice = Voice { speaker: speakers[0].id, speed: 1.0, volume: 1.0 };
    let wav = speech::voicevox_synthesize(&client, &base, "テストです", &voice).await.expect("synthesize");
    println!("wav bytes: {} (RIFF: {})", wav.len(), wav.starts_with(b"RIFF"));
    engine.close();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    println!("after close answering: {}", engine.refresh().await.running);
}
