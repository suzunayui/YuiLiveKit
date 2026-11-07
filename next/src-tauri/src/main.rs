// M0 prototype: a Tauri window plus the local server that OBS loads as one browser source.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{path::Path, sync::Arc};

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use serde::Serialize;
use tauri::Manager;

const OVERLAY_HTML: &str = include_str!("../../overlay/index.html");
const DEFAULT_PORT: u16 = 5000;
const CANVAS: (u32, u32) = (1920, 1080);

fn port() -> u16 {
    std::env::var("YUI_LIVE_KIT_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

fn overlay_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/overlay")
}

/// Hosts the server answers to. Checking Host stops DNS rebinding from reaching it through a web page.
#[derive(Clone)]
struct AllowedHosts(Arc<[String]>);

async fn check_host(State(allowed): State<AllowedHosts>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    match host {
        Some(host) if allowed.0.iter().any(|item| item == host) => next.run(request).await,
        _ => StatusCode::FORBIDDEN.into_response(),
    }
}

fn router(port: u16) -> Router {
    let allowed = AllowedHosts(Arc::from([
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
    ]));
    Router::new()
        .route("/overlay", get(|| async { Html(OVERLAY_HTML) }))
        .route("/health", get(|| async { "ok" }))
        .layer(middleware::from_fn_with_state(allowed, check_host))
}

#[derive(Serialize)]
struct ObsSource {
    url: String,
    width: u32,
    height: u32,
}

/// The OBS plugin reads this file to learn where the overlay is.
fn write_obs_source_file(dir: &Path, port: u16) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let source = ObsSource {
        url: overlay_url(port),
        width: CANVAS.0,
        height: CANVAS.1,
    };
    std::fs::write(dir.join("obs-source.json"), serde_json::to_vec_pretty(&source)?)
}

#[derive(Clone, Serialize)]
struct ServerState {
    url: String,
    error: Option<String>,
}

#[tauri::command]
fn server_state(state: tauri::State<'_, std::sync::Mutex<ServerState>>) -> ServerState {
    state.lock().unwrap().clone()
}

fn main() {
    let port = port();
    tauri::Builder::default()
        .manage(std::sync::Mutex::new(ServerState {
            url: overlay_url(port),
            error: None,
        }))
        .invoke_handler(tauri::generate_handler![server_state])
        .setup(move |app| {
            if let Err(error) = write_obs_source_file(&app.path().app_data_dir()?, port) {
                eprintln!("could not write obs-source.json: {error}");
            }
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let result = async {
                    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
                    axum::serve(listener, router(port)).await
                }
                .await;
                if let Err(error) = result {
                    let message = format!("ポート{port}を使えないため、OBSに表示できません: {error}");
                    eprintln!("{message}");
                    handle.state::<std::sync::Mutex<ServerState>>().lock().unwrap().error = Some(message);
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running YuiLiveKit");
}
