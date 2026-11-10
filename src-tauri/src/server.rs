//! The local server OBS loads as its one browser source. Read-only: changes go through Tauri commands.

use std::sync::Arc;

use axum::{
    extract::{
        ws::{rejection::WebSocketUpgradeRejection, Message, WebSocket, WebSocketUpgrade},
        Path, Request, State,
    },
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use tokio::sync::broadcast::error::RecvError;

use crate::core::{Core, Notice};

const OVERLAY_HTML: &str = include_str!("../../overlay/index.html");
const SNAPSHOT_SIZE: usize = 30;

#[derive(Clone)]
struct AppState {
    core: Arc<Core>,
    hosts: Arc<[String]>,
    origins: Arc<[String]>,
}

/// Rejects other Host headers, so a web page cannot reach the server through DNS rebinding.
async fn check_host(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let host = request.headers().get(header::HOST).and_then(|v| v.to_str().ok());
    match host {
        Some(host) if state.hosts.iter().any(|h| h == host) => next.run(request).await,
        _ => StatusCode::FORBIDDEN.into_response(),
    }
}

/// Browsers send Origin with WebSocket requests and do not apply CORS to them, so check it here.
fn origin_allowed(headers: &HeaderMap, origins: &[String]) -> bool {
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        None => true,
        Some(origin) => origins.iter().any(|o| o == origin),
    }
}

/// Binds without SO_REUSEADDR. Tokio's own bind sets it on Windows, which let a second
/// app take the same port silently instead of failing (K10). Must run inside a Tokio runtime.
pub fn bind(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    listener.set_nonblocking(true)?;
    tokio::net::TcpListener::from_std(listener)
}

pub fn router(core: Arc<Core>, port: u16) -> Router {
    let state = AppState {
        core,
        hosts: Arc::from([format!("127.0.0.1:{port}"), format!("localhost:{port}")]),
        origins: Arc::from([format!("http://127.0.0.1:{port}"), format!("http://localhost:{port}")]),
    };
    Router::new()
        .route("/overlay/{part}", get(overlay_page))
        .route("/overlay/ws", get(overlay_ws))
        .route("/health", get(|| async { "ok" }))
        .layer(middleware::from_fn_with_state(state.clone(), check_host))
        .with_state(state)
}

/// One page serves every part; it reads which one from its path.
async fn overlay_page(Path(part): Path<String>) -> Response {
    match crate::appearance::part(&part) {
        Some(_) => Html(OVERLAY_HTML).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The upgrade is taken as a Result so the origin is checked before anything else answers.
async fn overlay_ws(
    headers: HeaderMap,
    State(state): State<AppState>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if !origin_allowed(&headers, &state.origins) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match ws {
        Ok(ws) => ws.on_upgrade(move |socket| overlay_socket(socket, state.core)),
        Err(rejection) => rejection.into_response(),
    }
}

/// Everything the page needs to draw from scratch.
fn snapshot_json(core: &Core) -> String {
    serde_json::json!({
        "type": "snapshot",
        "messages": core.overlay_snapshot(SNAPSHOT_SIZE),
        "appearance": core.appearance(),
        "participants": core.participants(),
        "ohayo": core.ohayo(),
        "translations": core.translations(),
        "subtitle": core.last_subtitle().map(|(ja, en, at)| serde_json::json!({"ja": ja, "en": en, "age_ms": crate::core::now_ms() - at})),
    })
    .to_string()
}

async fn overlay_socket(mut socket: WebSocket, core: Arc<Core>) {
    let mut notices = core.subscribe();
    if socket.send(Message::Text(snapshot_json(&core).into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            notice = notices.recv() => {
                let text = match notice {
                    // Restored chat is for the settings window; the overlay showed it already.
                    Ok(Notice::Restored { .. })
                    | Ok(Notice::Status { .. })
                    | Ok(Notice::Speech { .. })
                    | Ok(Notice::SubtitleState(_))
                    | Ok(Notice::TranslationState(_))
                    | Ok(Notice::Runtime(_))
                    | Ok(Notice::Voicevox(_)) => continue,
                    Ok(notice) => serde_json::to_string(&notice).unwrap_or_default(),
                    // Too far behind: start over from the current state.
                    Err(RecvError::Lagged(_)) => snapshot_json(&core),
                    Err(RecvError::Closed) => break,
                };
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use axum::body::Body;
    use tower::ServiceExt;

    async fn status(host: &str, path: &str, origin: Option<&str>) -> StatusCode {
        let app = router(Core::new(Db::open_in_memory().unwrap()), 5000);
        let mut request = axum::http::Request::builder().uri(path).header(header::HOST, host);
        if let Some(origin) = origin {
            request = request
                .header(header::ORIGIN, origin)
                .header(header::CONNECTION, "upgrade")
                .header(header::UPGRADE, "websocket")
                .header("sec-websocket-version", "13")
                .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==");
        }
        app.oneshot(request.body(Body::empty()).unwrap()).await.unwrap().status()
    }

    #[tokio::test]
    async fn answers_only_local_hosts() {
        assert_eq!(status("127.0.0.1:5000", "/overlay/comments", None).await, StatusCode::OK);
        assert_eq!(status("127.0.0.1:5000", "/overlay/niconico", None).await, StatusCode::OK);
        assert_eq!(status("127.0.0.1:5000", "/overlay/unknown", None).await, StatusCode::NOT_FOUND);
        assert_eq!(status("localhost:5000", "/health", None).await, StatusCode::OK);
        assert_eq!(status("evil.example:5000", "/overlay/comments", None).await, StatusCode::FORBIDDEN);
        assert_eq!(status("127.0.0.1:5001", "/overlay/comments", None).await, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rejects_websockets_from_other_pages() {
        assert_eq!(status("127.0.0.1:5000", "/overlay/ws", Some("https://evil.example")).await, StatusCode::FORBIDDEN);
        assert_ne!(status("127.0.0.1:5000", "/overlay/ws", Some("http://127.0.0.1:5000")).await, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_port_in_use_is_reported_k10() {
        let first = bind(0).unwrap();
        let port = first.local_addr().unwrap().port();
        assert!(bind(port).is_err(), "a second server must not share the port");
        let held_by_tokio = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        assert!(bind(held_by_tokio.local_addr().unwrap().port()).is_err(), "also when the other side allows reuse");
    }

    #[test]
    fn the_overlay_never_parses_markup() {
        assert!(!OVERLAY_HTML.contains("innerHTML"));
        assert!(!OVERLAY_HTML.contains("insertAdjacentHTML"));
    }
}
