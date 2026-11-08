//! Runs chat collection, stores what arrives and tells the screens and the overlay.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::Serialize;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::{
    db::{Db, Shown},
    appearance::{self, Appearance},
    model::{Batch, ChatMessage, Platform},
    runtime::{Feature, Runtime, RuntimeState},
    speech::{self, AiSettings, Player, Queue, RateLimit, Secrets, Source, SpeechSettings, Utterance, Viewers},
    subtitles::{SubtitleSettings, SubtitleState, Subtitles},
    translation::{Translation, TranslationSettings, TranslationState},
    tools::{self, ClearMode, Ohayo, Participant, Support},
    twitch, youtube,
};

const APPEARANCE_KEY: &str = "appearance";
const PARTICIPANT_KEYWORD_KEY: &str = "participant_keyword";
const OHAYO_KEYWORDS_KEY: &str = "ohayo_keywords";
const SPEECH_KEY: &str = "speech";
const AI_KEY: &str = "ai";
const VIEWERS_KEY: &str = "viewers";
const SUBTITLES_KEY: &str = "subtitles";
const TRANSLATION_KEY: &str = "translation";
/// Chat sent this long before a start still counts as new, for clocks that differ a little.
const START_GRACE_MS: i64 = 2000;

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
    Appearance(Appearance),
    /// Something went wrong with reading aloud or AI replies.
    Speech { message: String },
    Subtitle { ja: String, en: String },
    SubtitleState(SubtitleState),
    Translation { key: String, ja: String },
    TranslationState(TranslationState),
    TranslationsCleared,
    Runtime(RuntimeState),
}

/// Subtitles, translation and what they need installed. Attached by the app once it knows its folders.
pub struct Media {
    pub runtime: Arc<Runtime>,
    pub subtitles: Arc<Subtitles>,
    pub translation: Arc<Translation>,
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
    /// Chat older than this is history and is never read aloud or answered.
    started_ms: i64,
    streams: Vec<i64>,
    cancel: Option<CancellationToken>,
    statuses: BTreeMap<&'static str, Status>,
    ohayo: Ohayo,
}

/// Where speech and AI replies go. Tests swap these for fakes.
pub struct Backends {
    pub player: Arc<dyn Player>,
    pub secrets: Box<dyn Secrets>,
    pub voicevox: String,
    pub openai: Option<String>,
}

impl Default for Backends {
    fn default() -> Self {
        Self {
            player: Arc::new(speech::SpeakerPlayer),
            secrets: Box::new(speech::CredentialManager),
            voicevox: speech::VOICEVOX_BASE.into(),
            openai: None,
        }
    }
}

#[derive(Default)]
struct Voices {
    speech: SpeechSettings,
    ai: AiSettings,
    viewers: Viewers,
}

pub struct Core {
    db: Mutex<Db>,
    notices: broadcast::Sender<Notice>,
    session: Mutex<Session>,
    settings: Mutex<ToolSettings>,
    voices: Mutex<Voices>,
    queue: Mutex<Queue>,
    wake: tokio::sync::Notify,
    rate: Mutex<RateLimit>,
    http: reqwest::Client,
    backends: Backends,
    /// For background work that outlives the call that started it.
    me: std::sync::Weak<Core>,
    media: Mutex<Option<Arc<Media>>>,
    media_settings: Mutex<(SubtitleSettings, TranslationSettings)>,
    /// Subtitles started by "start chat" also stop with it.
    subtitles_with_chat: Mutex<bool>,
    last_subtitle: Mutex<Option<(String, String, i64)>>,
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

fn load<T: serde::de::DeserializeOwned + Default>(db: &Db, key: &str) -> T {
    db.setting(key).ok().flatten().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

impl Core {
    pub fn new(db: Db) -> Arc<Self> {
        Self::with_backends(db, Backends::default())
    }

    pub fn with_backends(db: Db, backends: Backends) -> Arc<Self> {
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
        let voices = Voices {
            speech: load::<SpeechSettings>(&db, SPEECH_KEY).sanitized(),
            ai: load::<AiSettings>(&db, AI_KEY).sanitized(),
            viewers: load(&db, VIEWERS_KEY),
        };
        let media_settings = (load::<SubtitleSettings>(&db, SUBTITLES_KEY).sanitized(), load::<TranslationSettings>(&db, TRANSLATION_KEY).sanitized());
        let (notices, _) = broadcast::channel(1024);
        Arc::new_cyclic(|me| Self {
            media: Mutex::new(None),
            media_settings: Mutex::new(media_settings),
            subtitles_with_chat: Mutex::new(false),
            last_subtitle: Mutex::new(None),
            me: me.clone(),
            db: Mutex::new(db),
            notices,
            session: Mutex::new(Session { started_ms: i64::MAX, ..Session::default() }),
            settings: Mutex::new(ToolSettings { participant_keyword, ohayo_keywords }),
            voices: Mutex::new(voices),
            queue: Mutex::new(Queue::default()),
            wake: tokio::sync::Notify::new(),
            rate: Mutex::new(RateLimit::default()),
            http: speech::http(),
            backends,
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
            session.started_ms = now_ms() - START_GRACE_MS;
        }
        self.queue.lock().unwrap().clear();
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
            for s in &shown {
                let _ = db.record_emoji(&s.message);
            }
            let now = now_ms();
            let deleted: Vec<String> =
                batch.removals.iter().flat_map(|r| db.apply_removal(stream_id, r, now).unwrap_or_default()).collect();
            (shown, deleted, joined)
        };
        let may_change_ohayo = !deleted.is_empty() || shown.iter().any(|s| greeted(&s.message.text));
        let started_ms = self.session.lock().unwrap().started_ms;
        let translation = self.media().map(|m| m.translation.clone());
        for message in &shown {
            if message.message.created_ms >= started_ms {
                self.speak_message(stream_id, &message.message);
                self.maybe_answer(stream_id, &message.message);
                if let Some(translation) = &translation {
                    translation.enqueue(&message.key, &message.message.text, &message.message.kind);
                }
            }
        }
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

    // --- Appearance ---------------------------------------------------------------------

    pub fn appearance(&self) -> Appearance {
        appearance::from_setting(self.db().setting(APPEARANCE_KEY).ok().flatten())
    }

    pub fn save_appearance(&self, appearance: Appearance) -> Result<Appearance, String> {
        let appearance = appearance.sanitized();
        self.db().set_setting(APPEARANCE_KEY, &serde_json::to_value(&appearance).map_err(|e| e.to_string())?)?;
        self.notify(Notice::Appearance(appearance.clone()));
        Ok(appearance)
    }

    // --- Reading aloud and AI replies ---------------------------------------------------

    fn self_arc(&self) -> std::sync::Weak<Core> {
        self.me.clone()
    }

    pub fn speech_settings(&self) -> SpeechSettings {
        self.voices.lock().unwrap().speech.clone()
    }

    pub fn ai_settings(&self) -> AiSettings {
        self.voices.lock().unwrap().ai.clone()
    }

    pub fn viewers(&self) -> Viewers {
        self.voices.lock().unwrap().viewers.clone()
    }

    pub fn has_api_key(&self) -> bool {
        self.backends.secrets.get().is_some()
    }

    fn save<T: Serialize>(&self, key: &str, value: &T) -> Result<(), String> {
        self.db().set_setting(key, &serde_json::to_value(value).map_err(|e| e.to_string())?)
    }

    pub fn set_speech_settings(&self, settings: SpeechSettings) -> Result<SpeechSettings, String> {
        let settings = settings.sanitized();
        self.save(SPEECH_KEY, &settings)?;
        if !settings.enabled {
            self.queue.lock().unwrap().clear();
        }
        self.voices.lock().unwrap().speech = settings.clone();
        Ok(settings)
    }

    pub fn set_ai_settings(&self, settings: AiSettings) -> Result<AiSettings, String> {
        let settings = settings.sanitized();
        self.save(AI_KEY, &settings)?;
        self.voices.lock().unwrap().ai = settings.clone();
        Ok(settings)
    }

    /// Stores the key in the credential store, never in the database. `None` removes it.
    pub fn set_api_key(&self, key: Option<&str>) -> Result<bool, String> {
        self.backends.secrets.set(key)?;
        Ok(self.has_api_key())
    }

    /// Sets how a viewer's name is read, or clears it with an empty reading.
    pub fn set_viewer_name(&self, viewer: &str, reading: &str) -> Result<Viewers, String> {
        let mut voices = self.voices.lock().unwrap();
        let reading: String = reading.trim().chars().take(30).collect();
        if reading.is_empty() {
            voices.viewers.names.remove(viewer);
        } else {
            voices.viewers.names.insert(viewer.to_string(), reading);
        }
        let viewers = voices.viewers.clone();
        drop(voices);
        self.save(VIEWERS_KEY, &viewers)?;
        Ok(viewers)
    }

    pub fn set_blocked(&self, viewer: &str, blocked: bool) -> Result<Viewers, String> {
        let mut voices = self.voices.lock().unwrap();
        if blocked {
            voices.viewers.blocked.insert(viewer.to_string());
        } else {
            voices.viewers.blocked.remove(viewer);
        }
        let viewers = voices.viewers.clone();
        drop(voices);
        self.save(VIEWERS_KEY, &viewers)?;
        Ok(viewers)
    }

    fn readings(&self) -> BTreeMap<String, String> {
        self.db()
            .emoji()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| e.reading.map(|r| (e.code, r)))
            .collect()
    }

    fn sound_for(&self, message: &ChatMessage) -> Option<(String, f64)> {
        let emoji = self.db().emoji().unwrap_or_default();
        message.parts.iter().find_map(|part| match part {
            crate::model::Part::Emoji { alt, .. } => {
                emoji.iter().find(|e| &e.code == alt).and_then(|e| e.sound.clone().map(|s| (s, e.volume)))
            }
            _ => None,
        })
    }

    fn push(&self, utterance: Utterance) {
        let max = self.voices.lock().unwrap().speech.max_queue;
        self.queue.lock().unwrap().push(utterance, max);
        self.wake.notify_one();
    }

    fn speak_message(&self, stream_id: i64, message: &ChatMessage) {
        let (settings, viewers) = {
            let voices = self.voices.lock().unwrap();
            (voices.speech.clone(), voices.viewers.clone())
        };
        if !settings.enabled || !speech::allowed(message, &settings.ng_words, &viewers) {
            return;
        }
        let text = speech::speech_text(message, &settings, &viewers, &self.readings());
        let sound = self.sound_for(message);
        if text.is_none() && sound.is_none() {
            return;
        }
        self.push(Utterance {
            text: text.unwrap_or_default(),
            voice: settings.voice,
            source: Some(Source { stream_id, message: message.clone() }),
            sound,
        });
    }

    /// Asks OpenAI for a reply when a comment has a trigger word, within the rate limits.
    fn maybe_answer(&self, stream_id: i64, message: &ChatMessage) {
        let (ai, speech_settings, viewers) = {
            let voices = self.voices.lock().unwrap();
            (voices.ai.clone(), voices.speech.clone(), voices.viewers.clone())
        };
        if !ai.enabled || !speech::triggered(&message.text, &ai.triggers) {
            return;
        }
        if !speech::allowed(message, &speech_settings.ng_words, &viewers) {
            return;
        }
        let Some(key) = self.backends.secrets.get() else { return };
        let allowed = self.rate.lock().unwrap().try_take(Instant::now(), Duration::from_secs(ai.interval_seconds), ai.hourly_limit);
        if !allowed {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else { return };
        let request = speech::ai_request(&ai, &message.author, &message.text);
        let source = Source { stream_id, message: message.clone() };
        let http = self.http.clone();
        let base = self.backends.openai.clone();
        let core = self.self_arc();
        handle.spawn(async move {
            let Some(core) = core.upgrade() else { return };
            match speech::openai_reply(&http, base.as_deref(), &key, &request).await {
                Ok(reply) => {
                    let ng = core.speech_settings().ng_words;
                    if !speech::has_ng_word(&reply, &ng) {
                        core.push(Utterance { text: reply, voice: core.ai_settings().voice, source: Some(source), sound: None });
                    }
                }
                Err(message) => core.notify(Notice::Speech { message }),
            }
        });
    }

    /// Reads a line from the settings screen, to try a voice.
    pub fn speak_test(&self, text: &str, voice: speech::Voice) {
        let text: String = text.chars().take(speech::MAX_SPOKEN_CHARS).collect();
        if !text.trim().is_empty() {
            self.push(Utterance { text, voice, source: None, sound: None });
        }
    }

    /// Checked right before speaking: the source may have been deleted, or its viewer blocked
    /// or an NG word added since it was queued (K15).
    fn still_speakable(&self, utterance: &Utterance) -> bool {
        let (ng, viewers) = {
            let voices = self.voices.lock().unwrap();
            (voices.speech.ng_words.clone(), voices.viewers.clone())
        };
        if speech::has_ng_word(&utterance.text, &ng) {
            return false;
        }
        match &utterance.source {
            None => true,
            Some(source) => {
                !self.db().is_deleted(source.stream_id, &source.message.id).unwrap_or(true)
                    && speech::allowed(&source.message, &ng, &viewers)
            }
        }
    }

    async fn say(&self, utterance: Utterance) -> Result<(), String> {
        if let Some((path, volume)) = utterance.sound.clone() {
            let player = self.backends.player.clone();
            let audio = tokio::fs::read(&path).await.map_err(|e| format!("効果音を読めません: {e}"))?;
            tokio::task::spawn_blocking(move || player.play(audio, volume)).await.map_err(|e| e.to_string())??;
        }
        if utterance.text.is_empty() {
            return Ok(());
        }
        let wav = speech::voicevox_synthesize(&self.http, &self.backends.voicevox, &utterance.text, &utterance.voice).await?;
        let player = self.backends.player.clone();
        let volume = utterance.voice.volume;
        tokio::task::spawn_blocking(move || player.play(wav, volume)).await.map_err(|e| e.to_string())?
    }

    /// Speaks queued lines one at a time, for as long as the app runs.
    pub async fn run_speech(self: Arc<Self>) {
        loop {
            let next = self.queue.lock().unwrap().pop();
            let Some(utterance) = next else {
                self.wake.notified().await;
                continue;
            };
            if !self.still_speakable(&utterance) {
                continue;
            }
            if let Err(message) = self.say(utterance).await {
                self.notify(Notice::Speech { message });
            }
        }
    }

    pub async fn voicevox_speakers(&self) -> Result<Vec<speech::Speaker>, String> {
        speech::voicevox_speakers(&self.http, &self.backends.voicevox).await
    }

    // --- Subtitles and translation ------------------------------------------------------

    pub fn attach_media(&self, media: Media) {
        *self.media.lock().unwrap() = Some(Arc::new(media));
    }

    pub fn media(&self) -> Option<Arc<Media>> {
        self.media.lock().unwrap().clone()
    }

    pub fn media_settings(&self) -> (SubtitleSettings, TranslationSettings) {
        self.media_settings.lock().unwrap().clone()
    }

    /// What the overlay needs to redraw subtitles and translations from scratch.
    pub fn translations(&self) -> std::collections::HashMap<String, String> {
        self.media().map(|m| m.translation.results()).unwrap_or_default()
    }

    pub fn last_subtitle(&self) -> Option<(String, String, i64)> {
        self.last_subtitle.lock().unwrap().clone()
    }

    pub fn on_subtitle(&self, ja: String, en: String) {
        *self.last_subtitle.lock().unwrap() = Some((ja.clone(), en.clone(), now_ms()));
        self.notify(Notice::Subtitle { ja, en });
    }

    pub fn notify_media(&self, notice: Notice) {
        self.notify(notice);
    }

    pub fn clear_subtitle(&self) {
        *self.last_subtitle.lock().unwrap() = None;
        self.notify(Notice::Subtitle { ja: String::new(), en: String::new() });
    }

    pub fn set_subtitle_settings(&self, settings: SubtitleSettings) -> Result<SubtitleSettings, String> {
        let settings = settings.sanitized();
        self.save(SUBTITLES_KEY, &settings)?;
        if let Some(media) = self.media() {
            media.subtitles.set_mask(settings.mask_profanity);
        }
        self.media_settings.lock().unwrap().0 = settings.clone();
        Ok(settings)
    }

    /// Turning translation off while collecting stops it at once; turning it on starts it.
    pub fn set_translation_settings(&self, settings: TranslationSettings) -> Result<TranslationSettings, String> {
        let settings = settings.sanitized();
        self.save(TRANSLATION_KEY, &settings)?;
        let previous = std::mem::replace(&mut self.media_settings.lock().unwrap().1, settings.clone());
        let collecting = self.session.lock().unwrap().cancel.is_some();
        if collecting && previous != settings {
            if let Some(media) = self.media() {
                media.translation.stop();
                self.start_translation(&media, &settings);
            }
        }
        Ok(settings)
    }

    fn start_translation(&self, media: &Arc<Media>, settings: &TranslationSettings) {
        if !settings.enabled {
            return;
        }
        if media.runtime.ready(Feature::Translation) && !media.runtime.state().busy {
            media.translation.start(&settings.device);
        } else {
            self.notify(Notice::TranslationState(TranslationState {
                status: "コメント翻訳は未導入です。「コメント翻訳を導入」を押してください".into(),
                ..TranslationState::default()
            }));
        }
    }

    pub(crate) fn start_media_with_chat(&self) {
        let Some(media) = self.media() else { return };
        let (subtitle_settings, translation_settings) = self.media_settings();
        media.translation.stop();
        self.start_translation(&media, &translation_settings);
        if !subtitle_settings.start_with_chat || !media.runtime.ready(Feature::Speech) {
            return;
        }
        if subtitle_settings.microphone.is_none() {
            self.notify(Notice::SubtitleState(SubtitleState {
                status: "音声字幕タブでマイクを選ぶと、チャット取得開始で字幕も始まります".into(),
                ..media.subtitles.state()
            }));
            return;
        }
        *self.subtitles_with_chat.lock().unwrap() = true;
        let cuda = media.runtime.ready(Feature::Cuda);
        let Ok(handle) = tokio::runtime::Handle::try_current() else { return };
        let me = self.self_arc();
        handle.spawn(async move {
            // The worker may still be listing devices right after the app starts.
            for _ in 0..60 {
                if media.subtitles.state().ready {
                    break;
                }
                media.subtitles.prepare().await;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            if let Err(message) = media.subtitles.start(&subtitle_settings, cuda).await {
                if let Some(core) = me.upgrade() {
                    core.notify(Notice::SubtitleState(SubtitleState { status: format!("字幕を開始できません: {message}"), ..media.subtitles.state() }));
                }
            }
        });
    }

    /// Starts subtitles by themselves; they keep running when chat collection stops.
    pub async fn start_subtitles(&self) -> Result<(), String> {
        let media = self.media().ok_or("音声字幕は使えません")?;
        if media.runtime.state().busy {
            return Err("導入が終わるまでお待ちください".into());
        }
        *self.subtitles_with_chat.lock().unwrap() = false;
        let settings = self.media_settings().0;
        media.subtitles.start(&settings, media.runtime.ready(Feature::Cuda)).await
    }

    pub async fn stop_subtitles(&self) {
        *self.subtitles_with_chat.lock().unwrap() = false;
        if let Some(media) = self.media() {
            media.subtitles.stop().await;
        }
    }

    /// Lists microphones again by restarting the worker.
    pub async fn reload_devices(&self) {
        if let Some(media) = self.media() {
            if media.runtime.ready(Feature::Speech) && !media.subtitles.state().running {
                media.subtitles.close().await;
                media.subtitles.prepare().await;
            }
        }
    }

    /// Ends collection and every worker process; called when the app exits.
    pub fn shutdown(&self) {
        self.stop();
        if let Some(media) = self.media() {
            media.translation.stop();
            media.subtitles.kill();
            media.runtime.close();
        }
    }

    /// Installs a feature. Subtitles and translation stop first, since pip replaces files they load.
    pub async fn install_feature(&self, feature: Feature) -> Result<(), String> {
        let media = self.media().ok_or("追加機能は使えません")?;
        if media.subtitles.state().running || media.translation.state().active {
            return Err("字幕と翻訳を停止してから導入してください".into());
        }
        media.subtitles.close().await;
        let result = media.runtime.install(feature).await;
        if media.runtime.ready(Feature::Speech) {
            media.subtitles.prepare().await;
        }
        result
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
        self.start_media_with_chat();

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
        self.queue.lock().unwrap().clear();
        if let Some(media) = self.media() {
            media.translation.stop();
            if std::mem::take(&mut *self.subtitles_with_chat.lock().unwrap()) {
                let subtitles = media.subtitles.clone();
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move { subtitles.stop().await });
                }
            }
        }
        self.session.lock().unwrap().started_ms = i64::MAX;
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
    fn appearance_is_saved_cleaned_and_announced() {
        let core = Core::new(Db::open_in_memory().unwrap());
        let mut notices = core.subscribe();
        assert_eq!(core.appearance(), Appearance::default());
        let mut look = Appearance::default();
        look.comments.font_size = 999;
        let saved = core.save_appearance(look).unwrap();
        assert_eq!(saved.comments.font_size, 200);
        assert!(matches!(notices.try_recv().unwrap(), Notice::Appearance(a) if a.comments.font_size == 200));
        assert_eq!(core.appearance().comments.font_size, 200);
    }

    #[test]
    fn rejects_bad_inputs_before_starting() {
        let core = Core::new(Db::open_in_memory().unwrap());
        assert!(core.start("", "").unwrap_err().contains("どちらか"));
        assert!(core.start("https://example.com/x", "").unwrap_err().contains("YouTubeのURL"));
        assert!(core.start("", "a").unwrap_err().contains("Twitch"));
    }
}
