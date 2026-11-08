//! Runs the real subtitle and translation workers once, outside the app window.
//! Usage: media_probe <runtime folder> <model folder> <translation device, e.g. cuda:0>
//! Prints the probe's own sample translations and device counts only.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use yui_live_kit::{
    model::kind,
    runtime::{Feature, Paths, Runtime},
    subtitles::{Launch, Subtitles},
    translation::{Event, Translation},
};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(&args[1]);
    let models = PathBuf::from(&args[2]);
    let device = args.get(3).cloned().unwrap_or_else(|| "cpu".into());
    let paths = Paths { root: root.clone(), source: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../python") };
    let runtime = Runtime::new(paths.clone(), |_| {});
    for feature in [Feature::Speech, Feature::Cuda, Feature::Translation] {
        println!("{feature:?} ready: {}", runtime.ready(feature));
    }
    let env = |extra: &[(&str, String)]| {
        let mut env = HashMap::from([
            ("SUBTITLE_MODEL_DIR".to_string(), models.display().to_string()),
            ("HF_HUB_OFFLINE".to_string(), "1".to_string()),
        ]);
        env.extend(extra.iter().map(|(k, v)| (k.to_string(), v.clone())));
        env
    };

    let subtitles = Subtitles::new(
        Launch { python: paths.python(), script: paths.worker(Feature::Speech), cwd: root.clone(), env: env(&[]) },
        true,
        |_| {},
    );
    subtitles.prepare().await;
    for _ in 0..120 {
        if subtitles.state().ready {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let state = subtitles.state();
    println!("subtitles ready: {}, microphones: {}, gpus: {}, status: {}", state.ready, state.microphones.len(), state.gpus.len(), state.status);
    subtitles.close().await;

    let (sender, mut results) = tokio::sync::mpsc::unbounded_channel();
    let translation = Translation::new(
        Launch {
            python: paths.python(),
            script: paths.worker(Feature::Translation),
            cwd: root.clone(),
            env: env(&[("SUBTITLE_TRANSLATION_RUNTIME", paths.llama().display().to_string())]),
        },
        move |event| {
            let _ = sender.send(event);
        },
    );
    let started = std::time::Instant::now();
    translation.start(&device);
    let mut ready = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
    let mut queued = false;
    let mut translated = 0;
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, results.recv()).await {
        match event {
            Event::State(state) => {
                if state.ready && !ready {
                    ready = true;
                    println!("translation ready after {:.0}s on {device}", started.elapsed().as_secs_f64());
                }
                if !state.active {
                    println!("translation stopped: {}", state.status);
                    break;
                }
                if ready && !queued {
                    queued = true;
                    for (key, text) in [("a", "Hello, how are you?"), ("b", "こんにちは"), ("c", "Thank you for the stream!")] {
                        translation.enqueue(key, text, kind::TEXT);
                    }
                }
            }
            Event::Result { key, ja } => {
                println!("{key}: {ja}");
                translated += 1;
                if translated == 2 {
                    break;
                }
            }
            Event::Cleared => {}
        }
    }
    let state = translation.state();
    println!("translated {}, skipped {}, after {:.0}s", state.translated, state.skipped, started.elapsed().as_secs_f64());
    translation.stop();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = Arc::new(());
}
