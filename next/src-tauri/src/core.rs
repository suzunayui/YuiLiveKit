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
    layout::{self, Layout, Layouts},
    model::{Batch, Platform},
    tools::{self, ClearMode, Ohayo, Participant, Support},
    twitch, youtube,
};

const LAYOUTS_KEY: &str = "layouts";
const PARTICIPANT_KEYWORD_KEY: &str = "participant_keyword";
const OHAYO_KEYWORDS_KEY: &str = "ohayo_keywords";

/// What the screens and the overlay hear about.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Notice {
    /// A new collection started: drop what is shown.
    Reset,
    Message(Shown),
    Deleted { keys: Vec<String> },
    Status { platform: Platform, status: String, detail: Option<String> },
    Participants { items: Vec<Participant> },
    Ohayo(Ohayo),
    Layout { name: String, layout: Layout },
}

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub status: String,
    pub detail: Option<String>,
}

/// Keywords for the stream tools, kept in memory and saved in the settings table.
#[derive(Clone, Debug, Serialize)]
pub struct ToolSettings {
    pub participant_keyword: String,
    pub ohayo_keywords: Vec<String>,
}

#[derive(Default)]
struct Session {
    streams: Vec<i64>,
    cancel: Option<CancellationToken>,
    statuses: BTreeMap<&'static str, Status>,
    ohayo: Ohayo,
}

pub struct Core {
    db: Mutex<Db>,
    notices: broadcast::Sender<Notice>,
    session: Mutex<Session>,
    settings: Mutex<ToolSettings>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn clean_keywords(keywords: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for keyword in keywords.iter().map(|k| k.trim()).filter(|k| !k.is_empty()) {
        let keyword: String = keyword.chars().take(20).collect();
        if !out.contains(&keyword) && out.len() < 30 {
            out.push(keyword);
        }
    }
    out
}

impl Core {
    pub fn new(db: Db) -> Arc<Self> {
        let participant_keyword = db
            .setting(PARTICIPANT_KEYWORD_KEY)
            .ok()
            .flatten()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| tools::DEFAULT_PARTICIPANT_KEYWORD.into());
        let ohayo_keywords = db
            .setting(OHAYO_KEYWORDS_KEY)
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok())
            .unwrap_or_else(|| tools::DEFAULT_OHAYO_KEYWORDS.iter().map(|k| k.to_string()).collect());
        let (notices, _) = broadcast::channel(1024);
        Arc::new(Self {
            db: Mutex::new(db),
            notices,
            session: Mutex::new(Session::default()),
            settings: Mutex::new(ToolSettings { participant_keyword, ohayo_keywords }),
        })
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

    fn streams(&self) -> Vec<i64> {
        self.session.lock().unwrap().streams.clone()
    }

    pub fn tool_settings(&self) -> ToolSettings {
        self.settings.lock().unwrap().clone()
    }

    /// The newest messages of the current collection; empty before the first start.
    pub fn snapshot(&self, limit: usize) -> Vec<Shown> {
        self.db().recent(&self.streams(), limit).unwrap_or_default()
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
        {
            let mut session = self.session.lock().unwrap();
            if let Some(old) = session.cancel.replace(token.clone()) {
                old.cancel();
            }
            session.streams.clear();
            session.statuses.clear();
            session.ohayo = Ohayo::default();
        }
        self.notify(Notice::Reset);
        self.notify(Notice::Participants { items: Vec::new() });
        self.notify(Notice::Ohayo(Ohayo::default()));
        token
    }

    pub fn add_stream(&self, platform: Platform, external_id: &str) -> Option<i64> {
        let id = self.db().ensure_stream(platform, external_id, now_ms()).ok()?;
        {
            let mut session = self.session.lock().unwrap();
            if !session.streams.contains(&id) {
                session.streams.push(id);
            }
        }
        self.refresh_ohayo();
        Some(id)
    }

    /// Stores a batch, then announces new messages, removals and what they changed.
    pub fn ingest(&self, stream_id: i64, batch: Batch) {
        let settings = self.tool_settings();
        let greeted = |text: &str| settings.ohayo_keywords.iter().any(|k| text.contains(k.as_str()));
        let (shown, deleted, joined) = {
            let db = self.db();
            let shown: Vec<Shown> = batch.messages.iter().filter_map(|m| db.insert(stream_id, m).ok().flatten()).collect();
            let joined = shown
                .iter()
                .filter(|s| db.request_participant(s, &settings.participant_keyword).unwrap_or(false))
                .count();
            let now = now_ms();
            let deleted: Vec<String> =
                batch.removals.iter().flat_map(|r| db.apply_removal(stream_id, r, now).unwrap_or_default()).collect();
            (shown, deleted, joined)
        };
        let may_change_ohayo = !deleted.is_empty() || shown.iter().any(|s| greeted(&s.message.text));
        for message in shown {
            self.notify(Notice::Message(message));
        }
        if !deleted.is_empty() {
            self.notify(Notice::Deleted { keys: deleted });
        }
        if joined > 0 {
            self.notify_participants();
        }
        if may_change_ohayo {
            self.refresh_ohayo();
        }
    }

    // --- Participants -------------------------------------------------------------------

    pub fn participants(&self) -> Vec<Participant> {
        self.db().participants(&self.streams()).unwrap_or_default()
    }

    fn notify_participants(&self) {
        self.notify(Notice::Participants { items: self.participants() });
    }

    fn change_participants(&self, change: impl FnOnce(&Db, &[i64]) -> Result<(), String>) -> Result<Vec<Participant>, String> {
        let streams = self.streams();
        change(&self.db(), &streams)?;
        self.notify_participants();
        Ok(self.participants())
    }

    pub fn set_participant_status(&self, id: i64, status: &str) -> Result<Vec<Participant>, String> {
        self.change_participants(|db, _| db.set_participant_status(id, status))
    }

    pub fn clear_participants(&self, mode: ClearMode) -> Result<Vec<Participant>, String> {
        self.change_participants(|db, streams| db.clear_participants(streams, &mode))
    }

    pub fn reorder_participants(&self, ids: &[i64]) -> Result<Vec<Participant>, String> {
        self.change_participants(|db, streams| db.reorder_participants(streams, ids))
    }

    // --- Ohayo --------------------------------------------------------------------------

    pub fn ohayo(&self) -> Ohayo {
        self.session.lock().unwrap().ohayo.clone()
    }

    fn refresh_ohayo(&self) {
        let keywords = self.tool_settings().ohayo_keywords;
        let Ok(next) = self.db().ohayo(&self.streams(), &keywords) else { return };
        let changed = {
            let mut session = self.session.lock().unwrap();
            let changed = session.ohayo != next;
            session.ohayo = next.clone();
            changed
        };
        if changed {
            self.notify(Notice::Ohayo(next));
        }
    }

    pub fn adjust_ohayo(&self, delta: i64) -> Result<Ohayo, String> {
        let keywords = self.tool_settings().ohayo_keywords;
        self.db().adjust_ohayo(&self.streams(), &keywords, delta)?;
        self.refresh_ohayo();
        Ok(self.ohayo())
    }

    pub fn reset_ohayo(&self) -> Result<Ohayo, String> {
        self.db().reset_ohayo(&self.streams(), now_ms())?;
        self.refresh_ohayo();
        Ok(self.ohayo())
    }

    pub fn set_tool_settings(&self, participant_keyword: &str, ohayo_keywords: &[String]) -> Result<ToolSettings, String> {
        let keyword: String = participant_keyword.trim().chars().take(20).collect();
        if keyword.is_empty() {
            return Err("参加管理のキーワードを入力してください".into());
        }
        let keywords = clean_keywords(ohayo_keywords);
        {
            let db = self.db();
            db.set_setting(PARTICIPANT_KEYWORD_KEY, &serde_json::json!(keyword))?;
            db.set_setting(OHAYO_KEYWORDS_KEY, &serde_json::json!(keywords))?;
        }
        *self.settings.lock().unwrap() = ToolSettings { participant_keyword: keyword, ohayo_keywords: keywords };
        self.refresh_ohayo();
        Ok(self.tool_settings())
    }

    // --- Support ------------------------------------------------------------------------

    pub fn support(&self) -> Vec<Support> {
        self.db().support(&self.streams()).unwrap_or_default()
    }

    pub fn set_support_read(&self, row_ids: &[i64], read: bool) -> Result<Vec<Support>, String> {
        self.db().set_support_read(row_ids, read, now_ms())?;
        Ok(self.support())
    }

    // --- Layouts ------------------------------------------------------------------------

    pub fn layouts(&self) -> Layouts {
        layout::from_setting(self.db().setting(LAYOUTS_KEY).ok().flatten())
    }

    pub fn layout(&self, name: &str) -> Layout {
        let mut layouts = self.layouts();
        layouts.remove(name).unwrap_or_else(|| layouts.remove(layout::DEFAULT_NAME).unwrap_or_default())
    }

    pub fn save_layout(&self, name: &str, layout: Layout) -> Result<Layouts, String> {
        if !layout::valid_name(name) {
            return Err("配置の名前は半角英数字、-、_ の32文字までにしてください".into());
        }
        let layout = layout.sanitized();
        let mut layouts = self.layouts();
        layouts.insert(name.to_string(), layout.clone());
        self.db().set_setting(LAYOUTS_KEY, &serde_json::to_value(&layouts).map_err(|e| e.to_string())?)?;
        self.notify(Notice::Layout { name: name.to_string(), layout });
        Ok(layouts)
    }

    pub fn delete_layout(&self, name: &str) -> Result<Layouts, String> {
        if name == layout::DEFAULT_NAME {
            return Err("標準の配置は消せません".into());
        }
        let mut layouts = self.layouts();
        layouts.remove(name);
        self.db().set_setting(LAYOUTS_KEY, &serde_json::to_value(&layouts).map_err(|e| e.to_string())?)?;
        // Scenes still using the name fall back to the default layout.
        self.notify(Notice::Layout { name: name.to_string(), layout: self.layout(layout::DEFAULT_NAME) });
        Ok(layouts)
    }

    // --- Collection ---------------------------------------------------------------------

    /// Validates the inputs, then collects from each given platform. Needs a Tokio runtime.
    pub fn start(self: &Arc<Self>, youtube_input: &str, twitch_input: &str) -> Result<(), String> {
        let youtube_target =
            (!youtube_input.trim().is_empty()).then(|| youtube::parse_target(youtube_input)).transpose()?;
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

    fn message(id: &str, author: &str, text: &str) -> ChatMessage {
        ChatMessage {
            platform: Platform::Youtube,
            id: id.into(),
            author_id: Some(author.into()),
            author: author.into(),
            icon: None,
            kind: kind::TEXT.into(),
            text: text.into(),
            parts: vec![Part::Text { text: text.into() }],
            amount_micros: None,
            currency: None,
            amount_text: None,
            colors: None,
            badges: Badges::default(),
            created_ms: now_ms(),
        }
    }

    fn batch(messages: Vec<ChatMessage>) -> Batch {
        Batch { messages, removals: vec![] }
    }

    #[test]
    fn shows_only_the_current_collection() {
        let core = Core::new(Db::open_in_memory().unwrap());
        core.reset_session();
        let old = core.add_stream(Platform::Youtube, "old").unwrap();
        core.ingest(old, batch(vec![message("before", "UC1", "hi")]));
        assert_eq!(core.snapshot(10).len(), 1);
        core.reset_session();
        assert!(core.snapshot(10).is_empty(), "a new start does not show the last stream");
        let new = core.add_stream(Platform::Youtube, "new").unwrap();
        core.ingest(new, batch(vec![message("now", "UC1", "hi")]));
        assert_eq!(core.snapshot(10).iter().map(|s| s.message.id.as_str()).collect::<Vec<_>>(), vec!["now"]);
    }

    #[test]
    fn announces_messages_then_removals() {
        let core = Core::new(Db::open_in_memory().unwrap());
        let mut notices = core.subscribe();
        let stream = core.add_stream(Platform::Youtube, "v").unwrap();
        core.ingest(stream, Batch { messages: vec![message("a", "UC1", "hi")], removals: vec![Removal::Message("a".into())] });
        assert!(matches!(notices.try_recv().unwrap(), Notice::Message(m) if m.key == "youtube:a"));
        assert!(matches!(notices.try_recv().unwrap(), Notice::Deleted { keys } if keys == ["youtube:a"]));
    }

    #[test]
    fn participants_and_greetings_follow_the_chat() {
        let core = Core::new(Db::open_in_memory().unwrap());
        core.reset_session();
        let stream = core.add_stream(Platform::Youtube, "v").unwrap();
        let mut notices = core.subscribe();
        core.ingest(stream, batch(vec![message("1", "UC1", "参加希望！おはよう"), message("2", "UC1", "参加希望")]));
        let mut seen_participants = None;
        let mut seen_ohayo = None;
        while let Ok(notice) = notices.try_recv() {
            match notice {
                Notice::Participants { items } => seen_participants = Some(items.len()),
                Notice::Ohayo(o) => seen_ohayo = Some(o.count),
                _ => {}
            }
        }
        assert_eq!((seen_participants, seen_ohayo), (Some(1), Some(1)));
        let id = core.participants()[0].id;
        assert_eq!(core.set_participant_status(id, "done").unwrap()[0].status, "done");
        assert_eq!(core.adjust_ohayo(2).unwrap().count, 3);
        assert_eq!(core.reset_ohayo().unwrap().count, 0);
    }

    #[test]
    fn keywords_can_change_and_are_cleaned() {
        let core = Core::new(Db::open_in_memory().unwrap());
        assert!(core.set_tool_settings("  ", &[]).is_err());
        let settings = core.set_tool_settings(" 参加 ", &[" おはよ ".into(), "".into(), "おはよ".into()]).unwrap();
        assert_eq!(settings.participant_keyword, "参加");
        assert_eq!(settings.ohayo_keywords, ["おはよ"]);
    }

    #[test]
    fn layouts_are_saved_validated_and_announced() {
        let core = Core::new(Db::open_in_memory().unwrap());
        let mut notices = core.subscribe();
        let mut game = Layout::default();
        game.comments.area.x = 300;
        assert!(core.save_layout("bad name", game.clone()).is_err());
        let layouts = core.save_layout("game", game).unwrap();
        assert_eq!(layouts.keys().collect::<Vec<_>>(), ["default", "game"]);
        assert!(matches!(notices.try_recv().unwrap(), Notice::Layout { name, layout } if name == "game" && layout.comments.area.x == 300));
        assert_eq!(core.layout("missing"), Layout::default());
        assert!(core.delete_layout("default").is_err());
        assert_eq!(core.delete_layout("game").unwrap().len(), 1);
    }

    #[test]
    fn rejects_bad_inputs_before_starting() {
        let core = Core::new(Db::open_in_memory().unwrap());
        assert!(core.start("", "").unwrap_err().contains("どちらか"));
        assert!(core.start("https://example.com/x", "").unwrap_err().contains("YouTubeのURL"));
        assert!(core.start("", "a").unwrap_err().contains("Twitch"));
    }
}
