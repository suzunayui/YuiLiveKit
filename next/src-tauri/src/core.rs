//! Runs chat collection, stores what arrives and tells the screens and the overlay.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use serde::Serialize;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::{
    db::{Db, Shown},
    model::{Batch, Platform},
    twitch, youtube,
};

/// What the screens and the overlay hear about.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Notice {
    /// A new collection started: drop what is shown.
    Reset,
    Message(Shown),
    Deleted { keys: Vec<String> },
    Status { platform: Platform, status: String, detail: Option<String> },
}

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub status: String,
    pub detail: Option<String>,
}

#[derive(Default)]
struct Session {
    streams: Vec<i64>,
    cancel: Option<CancellationToken>,
    statuses: BTreeMap<&'static str, Status>,
}

pub struct Core {
    db: Mutex<Db>,
    notices: broadcast::Sender<Notice>,
    session: Mutex<Session>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl Core {
    pub fn new(db: Db) -> Arc<Self> {
        let (notices, _) = broadcast::channel(1024);
        Arc::new(Self { db: Mutex::new(db), notices, session: Mutex::new(Session::default()) })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Notice> {
        self.notices.subscribe()
    }

    fn notify(&self, notice: Notice) {
        let _ = self.notices.send(notice);
    }

    pub fn db(&self) -> std::sync::MutexGuard<'_, Db> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The newest messages of the current collection; empty before the first start.
    pub fn snapshot(&self, limit: usize) -> Vec<Shown> {
        let streams = self.session.lock().unwrap().streams.clone();
        self.db().recent(&streams, limit).unwrap_or_default()
    }

    pub fn statuses(&self) -> BTreeMap<&'static str, Status> {
        self.session.lock().unwrap().statuses.clone()
    }

    /// Records the status and announces it only when it changed (polls report "running" every time).
    pub fn set_status(&self, platform: Platform, status: &str, detail: Option<String>) {
        let next = Status { status: status.to_string(), detail: detail.clone() };
        let previous = self.session.lock().unwrap().statuses.insert(platform.as_str(), next);
        let unchanged = previous.is_some_and(|p| p.status == status && p.detail == detail);
        if !unchanged {
            self.notify(Notice::Status { platform, status: status.to_string(), detail });
        }
    }

    /// Starts a new collection: only its streams are shown from now on.
    pub fn reset_session(&self) -> CancellationToken {
        let token = CancellationToken::new();
        let mut session = self.session.lock().unwrap();
        if let Some(old) = session.cancel.replace(token.clone()) {
            old.cancel();
        }
        session.streams.clear();
        session.statuses.clear();
        drop(session);
        self.notify(Notice::Reset);
        token
    }

    pub fn add_stream(&self, platform: Platform, external_id: &str) -> Option<i64> {
        let id = self.db().ensure_stream(platform, external_id, now_ms()).ok()?;
        let mut session = self.session.lock().unwrap();
        if !session.streams.contains(&id) {
            session.streams.push(id);
        }
        Some(id)
    }

    /// Stores a batch, then announces new messages followed by removals.
    pub fn ingest(&self, stream_id: i64, batch: Batch) {
        let (shown, deleted) = {
            let db = self.db();
            let shown: Vec<Shown> = batch
                .messages
                .iter()
                .filter_map(|m| db.insert(stream_id, m).ok().flatten())
                .collect();
            let now = now_ms();
            let deleted: Vec<String> = batch
                .removals
                .iter()
                .flat_map(|r| db.apply_removal(stream_id, r, now).unwrap_or_default())
                .collect();
            (shown, deleted)
        };
        for message in shown {
            self.notify(Notice::Message(message));
        }
        if !deleted.is_empty() {
            self.notify(Notice::Deleted { keys: deleted });
        }
    }

    /// Validates the inputs, then collects from each given platform. Needs a Tokio runtime.
    pub fn start(self: &Arc<Self>, youtube_input: &str, twitch_input: &str) -> Result<(), String> {
        let youtube_target = (!youtube_input.trim().is_empty()).then(|| youtube::parse_target(youtube_input)).transpose()?;
        let twitch_channel = (!twitch_input.trim().is_empty()).then(|| twitch::parse_channel(twitch_input)).transpose()?;
        if youtube_target.is_none() && twitch_channel.is_none() {
            return Err("YouTubeかTwitchのどちらかを入力してください".into());
        }
        let token = self.reset_session();

        if let Some(target) = youtube_target {
            let core = self.clone();
            let token = token.clone();
            core.set_status(Platform::Youtube, "starting", None);
            tokio::spawn(async move {
                let mut stream_id = None;
                let result = youtube::run(target, token.clone(), |event| match event {
                    youtube::Event::Stream { video_id } => stream_id = core.add_stream(Platform::Youtube, &video_id),
                    youtube::Event::Batch(batch) => {
                        if let Some(id) = stream_id {
                            core.ingest(id, batch);
                        }
                    }
                    youtube::Event::Status(status) => core.set_status(Platform::Youtube, status, None),
                })
                .await;
                if !token.is_cancelled() {
                    core.set_status(Platform::Youtube, "stopped", result.err());
                }
            });
        }

        if let Some(channel) = twitch_channel {
            let core = self.clone();
            let token = token.clone();
            core.set_status(Platform::Twitch, "starting", None);
            tokio::spawn(async move {
                // One stream per collection, so reconnects do not count as new streams.
                let external_id = format!("{channel}:{}", now_ms());
                let mut stream_id = None;
                twitch::run(channel, token.clone(), |event| match event {
                    twitch::Event::Connected => {
                        if stream_id.is_none() {
                            stream_id = core.add_stream(Platform::Twitch, &external_id);
                        }
                    }
                    twitch::Event::Batch(batch) => {
                        if let Some(id) = stream_id {
                            core.ingest(id, batch);
                        }
                    }
                    twitch::Event::Status(status) => core.set_status(Platform::Twitch, status, None),
                })
                .await;
            });
        }
        Ok(())
    }

    /// Stops collecting. What was collected stays on the overlay until the next start.
    pub fn stop(&self) {
        let platforms: Vec<Platform> = {
            let mut session = self.session.lock().unwrap();
            if let Some(token) = session.cancel.take() {
                token.cancel();
            }
            session.statuses.keys().filter_map(|p| Platform::parse(p)).collect()
        };
        for platform in platforms {
            self.set_status(platform, "stopped", None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{kind, Badges, ChatMessage, Part, Removal};

    fn message(id: &str) -> ChatMessage {
        ChatMessage {
            platform: Platform::Youtube,
            id: id.into(),
            author_id: Some("UC1".into()),
            author: "A".into(),
            icon: None,
            kind: kind::TEXT.into(),
            text: id.into(),
            parts: vec![Part::Text { text: id.into() }],
            amount_micros: None,
            currency: None,
            amount_text: None,
            colors: None,
            badges: Badges::default(),
            created_ms: 1,
        }
    }

    #[test]
    fn shows_only_the_current_collection() {
        let core = Core::new(Db::open_in_memory().unwrap());
        core.reset_session();
        let old = core.add_stream(Platform::Youtube, "old").unwrap();
        core.ingest(old, Batch { messages: vec![message("before")], removals: vec![] });
        assert_eq!(core.snapshot(10).len(), 1);
        core.reset_session();
        assert!(core.snapshot(10).is_empty(), "a new start does not show the last stream");
        let new = core.add_stream(Platform::Youtube, "new").unwrap();
        core.ingest(new, Batch { messages: vec![message("now")], removals: vec![] });
        assert_eq!(core.snapshot(10).iter().map(|s| s.message.id.as_str()).collect::<Vec<_>>(), vec!["now"]);
    }

    #[test]
    fn announces_messages_then_removals() {
        let core = Core::new(Db::open_in_memory().unwrap());
        let mut notices = core.subscribe();
        let stream = core.add_stream(Platform::Youtube, "v").unwrap();
        core.ingest(stream, Batch { messages: vec![message("a")], removals: vec![Removal::Message("a".into())] });
        assert!(matches!(notices.try_recv().unwrap(), Notice::Message(m) if m.key == "youtube:a"));
        assert!(matches!(notices.try_recv().unwrap(), Notice::Deleted { keys } if keys == ["youtube:a"]));
    }

    #[test]
    fn rejects_bad_inputs_before_starting() {
        let core = Core::new(Db::open_in_memory().unwrap());
        assert!(core.start("", "").unwrap_err().contains("どちらか"));
        assert!(core.start("https://example.com/x", "").unwrap_err().contains("YouTubeのURL"));
        assert!(core.start("", "a").unwrap_err().contains("Twitch"));
    }
}
