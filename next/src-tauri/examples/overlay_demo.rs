//! Runs the overlay server with an in-memory database and takes chat from stdin, one JSON per line:
//!   {"op":"start","stream":"v1"}           begin a new collection on a YouTube stream
//!   {"op":"message","id":"a","author":"A","author_id":"UC1","text":"hi","icon":"...","amount_text":"¥1,000"}
//!   {"op":"remove","id":"a"}   {"op":"remove_author","author_id":"UC1"}
//! Used by tests/overlay.test.cjs; nothing here reaches YouTube or Twitch.

use std::io::BufRead;

use serde_json::Value;
use yui_live_kit::{
    core::{now_ms, Core},
    db::Db,
    model::{kind, Badges, Batch, ChatMessage, Part, Platform, Removal},
};

fn text(value: &Value, key: &str) -> Option<String> {
    value[key].as_str().map(str::to_string)
}

#[tokio::main]
async fn main() {
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(15030);
    let core = Core::new(Db::open_in_memory().expect("database"));
    let listener = yui_live_kit::server::bind(port).expect("port");
    let server = core.clone();
    tokio::spawn(async move { axum::serve(listener, yui_live_kit::server::router(server, port)).await });
    println!("ready {port}");

    let (lines_tx, mut lines) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if lines_tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut stream = None;
    while let Some(line) = lines.recv().await {
        let Ok(command) = serde_json::from_str::<Value>(&line) else { continue };
        match command["op"].as_str() {
            Some("start") => {
                core.reset_session();
                stream = core.add_stream(Platform::Youtube, &text(&command, "stream").unwrap_or_default());
            }
            Some("message") => {
                let body = text(&command, "text").unwrap_or_default();
                let amount_text = text(&command, "amount_text");
                let amount_micros = amount_text.as_deref().and_then(yui_live_kit::youtube::parse_yen);
                let message = ChatMessage {
                    platform: Platform::Youtube,
                    id: text(&command, "id").unwrap_or_default(),
                    author_id: text(&command, "author_id"),
                    author: text(&command, "author").unwrap_or_default(),
                    icon: text(&command, "icon").and_then(|u| yui_live_kit::model::allowed_image(&u)),
                    kind: text(&command, "kind").unwrap_or_else(|| kind::TEXT.into()),
                    parts: vec![Part::Text { text: body.clone() }],
                    text: body,
                    currency: amount_micros.map(|_| "JPY".into()),
                    amount_micros,
                    amount_text,
                    colors: None,
                    badges: Badges::default(),
                    created_ms: now_ms(),
                };
                if let Some(id) = stream {
                    core.ingest(id, Batch { messages: vec![message], removals: vec![] });
                }
            }
            Some("save_layout") => {
                let name = text(&command, "name").unwrap_or_default();
                let layout = serde_json::from_value(command["layout"].clone()).unwrap_or_default();
                if let Err(error) = core.save_layout(&name, layout) {
                    eprintln!("save_layout: {error}");
                }
            }
            Some(op @ ("remove" | "remove_author")) => {
                let removal = if op == "remove" {
                    Removal::Message(text(&command, "id").unwrap_or_default())
                } else {
                    Removal::Author(text(&command, "author_id").unwrap_or_default())
                };
                if let Some(id) = stream {
                    core.ingest(id, Batch { messages: vec![], removals: vec![removal] });
                }
            }
            _ => {}
        }
        println!("ok");
    }
    // Keep serving when stdin closes, so the demo can also stand in for "another app on the port".
    std::future::pending::<()>().await;
}
