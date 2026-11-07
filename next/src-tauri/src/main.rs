// Tauri glue: the window, its commands, and the local server for OBS.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::Serialize;
use tauri::{Emitter, Manager};
use yui_live_kit::{core::Core, db::Db};

const DEFAULT_PORT: u16 = 5000;
const CANVAS: (u32, u32) = (1920, 1080);
const LIST_SIZE: usize = 300;

fn port() -> u16 {
    std::env::var("YUI_LIVE_KIT_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_PORT)
}

fn overlay_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/overlay")
}

/// The OBS plugin reads this file to learn where the overlay is.
fn write_obs_source_file(dir: &Path, port: u16) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let source = serde_json::json!({"url": overlay_url(port), "width": CANVAS.0, "height": CANVAS.1});
    std::fs::write(dir.join("obs-source.json"), serde_json::to_vec_pretty(&source)?)
}

/// comments.db files of the Electron version, newest data first.
fn legacy_candidates() -> Vec<String> {
    let Ok(app_data) = std::env::var("APPDATA") else { return Vec::new() };
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = ["YuiLiveKit", "yt-overlay", "YouTubeOverlay"]
        .iter()
        .map(|name| Path::new(&app_data).join(name).join("comments.db"))
        .filter_map(|path| Some((std::fs::metadata(&path).ok()?.modified().ok()?, path)))
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, path)| path.display().to_string()).collect()
}

struct ServerError(Mutex<Option<String>>);

#[derive(Serialize)]
struct AppState {
    overlay_url: String,
    server_error: Option<String>,
    statuses: std::collections::BTreeMap<&'static str, yui_live_kit::core::Status>,
    messages: Vec<yui_live_kit::db::Shown>,
    legacy_imported: bool,
    legacy_candidates: Vec<String>,
}

#[tauri::command]
fn get_state(core: tauri::State<'_, Arc<Core>>, error: tauri::State<'_, ServerError>) -> AppState {
    AppState {
        overlay_url: overlay_url(port()),
        server_error: error.0.lock().unwrap().clone(),
        statuses: core.statuses(),
        messages: core.snapshot(LIST_SIZE),
        legacy_imported: core.db().legacy_imported().unwrap_or(false),
        legacy_candidates: legacy_candidates(),
    }
}

#[tauri::command]
async fn start_chat(core: tauri::State<'_, Arc<Core>>, youtube: String, twitch: String) -> Result<(), String> {
    core.start(&youtube, &twitch)
}

#[tauri::command]
fn stop_chat(core: tauri::State<'_, Arc<Core>>) {
    core.stop();
}

#[tauri::command]
async fn import_legacy(core: tauri::State<'_, Arc<Core>>, path: String) -> Result<yui_live_kit::db::ImportSummary, String> {
    if !legacy_candidates().contains(&path) {
        return Err("取り込めるのは、見つかった以前のデータベースだけです".into());
    }
    let core = core.inner().clone();
    tauri::async_runtime::spawn_blocking(move || core.db().import_legacy(Path::new(&path)))
        .await
        .map_err(|e| e.to_string())?
}

fn main() {
    let port = port();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .manage(ServerError(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![get_state, start_chat, stop_chat, import_legacy])
        .setup(move |app| {
            let data_dir = app.path().app_data_dir()?;
            if let Err(error) = write_obs_source_file(&data_dir, port) {
                eprintln!("could not write obs-source.json: {error}");
            }
            let core = Core::new(Db::open(&data_dir.join("livekit.db"))?);
            app.manage(core.clone());

            let handle = app.handle().clone();
            let mut notices = core.subscribe();
            tauri::async_runtime::spawn(async move {
                while let Ok(notice) = notices.recv().await {
                    let _ = handle.emit("chat", &notice);
                }
            });

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let result = async {
                    let listener = yui_live_kit::server::bind(port)?;
                    axum::serve(listener, yui_live_kit::server::router(core, port)).await
                }
                .await;
                if let Err(error) = result {
                    let message = format!("ポート{port}を使えないため、OBSに表示できません。ほかのアプリが使っていないか確認してください（{error}）");
                    eprintln!("{message}");
                    *handle.state::<ServerError>().0.lock().unwrap() = Some(message.clone());
                    let _ = handle.emit("server-error", message);
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running YuiLiveKit");
}
