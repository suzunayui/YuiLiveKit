//! Japanese and English subtitles from the microphone, through the faster-whisper worker
//! (`python/subtitles/worker.py`). Nothing is recorded or saved.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::worker::Worker;

pub const MODELS: [&str; 4] = ["large-v3", "medium", "small", "base"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubtitleSettings {
    pub microphone: Option<u32>,
    /// "cpu" or "cuda:<n>".
    pub device: String,
    pub model: String,
    pub threshold: f64,
    pub start_with_chat: bool,
    pub mask_profanity: bool,
}

impl Default for SubtitleSettings {
    fn default() -> Self {
        Self { microphone: None, device: "cpu".into(), model: "large-v3".into(), threshold: 0.5, start_with_chat: true, mask_profanity: true }
    }
}

impl SubtitleSettings {
    pub fn sanitized(mut self) -> Self {
        if !valid_device(&self.device) {
            self.device = "cpu".into();
        }
        if !MODELS.contains(&self.model.as_str()) {
            self.model = "large-v3".into();
        }
        self.threshold = if self.threshold.is_finite() { self.threshold.clamp(0.3, 0.8) } else { 0.5 };
        self
    }
}

pub fn valid_device(device: &str) -> bool {
    device == "cpu" || device.strip_prefix("cuda:").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: u32,
    pub name: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SubtitleState {
    pub ready: bool,
    pub running: bool,
    pub status: String,
    pub microphones: Vec<Device>,
    pub gpus: Vec<Device>,
    pub level: f64,
}

pub enum Event {
    State(SubtitleState),
    Subtitle { ja: String, en: String },
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Masks one English swear word and its forms, plus the katakana spelling (f**k, フ＊＊ク).
/// Words merely containing the letters, such as "firetruck", stay as they are.
pub fn mask_profanity(text: &str) -> String {
    let pattern = regex::Regex::new(r"(?i)(?:mother)?fuck(?:ing|ed|ers|er|s)?").expect("pattern");
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for found in pattern.find_iter(text) {
        let before = text[..found.start()].chars().next_back();
        let after = text[found.end()..].chars().next();
        out.push_str(&text[last..found.start()]);
        if before.is_some_and(is_word_char) || after.is_some_and(is_word_char) {
            out.push_str(found.as_str());
        } else {
            let word = found.as_str();
            let at = word.to_ascii_lowercase().find("uc").unwrap_or(0);
            out.push_str(&word[..at]);
            out.push_str("**");
            out.push_str(&word[at + 2..]);
        }
        last = found.end();
    }
    out.push_str(&text[last..]);
    out.replace("ファック", "フ＊＊ク")
}

/// Where the worker runs and where models go.
#[derive(Clone)]
pub struct Launch {
    pub python: PathBuf,
    pub script: PathBuf,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
}

pub struct Subtitles {
    launch: Launch,
    state: Mutex<SubtitleState>,
    worker: tokio::sync::Mutex<Option<Worker>>,
    publish: Arc<dyn Fn(Event) + Send + Sync>,
    mask: Mutex<bool>,
    last_level: Mutex<Instant>,
    pid: Mutex<Option<u32>>,
}

impl Subtitles {
    pub fn new(launch: Launch, mask: bool, publish: impl Fn(Event) + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            launch,
            state: Mutex::new(SubtitleState { status: "音声字幕は未導入です。「音声字幕を導入」を押してください".into(), ..SubtitleState::default() }),
            worker: tokio::sync::Mutex::new(None),
            publish: Arc::new(publish),
            mask: Mutex::new(mask),
            last_level: Mutex::new(Instant::now()),
            pid: Mutex::new(None),
        })
    }

    pub fn state(&self) -> SubtitleState {
        self.state.lock().unwrap().clone()
    }

    pub fn set_mask(&self, mask: bool) {
        *self.mask.lock().unwrap() = mask;
    }

    fn update(&self, change: impl FnOnce(&mut SubtitleState)) {
        let snapshot = {
            let mut state = self.state.lock().unwrap();
            change(&mut state);
            state.clone()
        };
        (self.publish)(Event::State(snapshot));
    }

    /// Starts the worker so it can list microphones. Safe to call again after a failure.
    pub async fn prepare(self: &Arc<Self>) {
        let mut slot = self.worker.lock().await;
        if slot.is_some() {
            return;
        }
        self.update(|s| s.status = "マイクを準備中".into());
        let args = vec!["-u".to_string(), self.launch.script.display().to_string()];
        match Worker::spawn(&self.launch.python, &args, &self.launch.env, &self.launch.cwd) {
            Ok((worker, mut events)) => {
                *self.pid.lock().unwrap() = worker.pid();
                *slot = Some(worker);
                let me = Arc::downgrade(self);
                tokio::spawn(async move {
                    while let Some(event) = events.recv().await {
                        let Some(me) = me.upgrade() else { return };
                        me.handle(event);
                    }
                    if let Some(me) = me.upgrade() {
                        *me.worker.lock().await = None;
                        me.update(|s| {
                            s.ready = false;
                            s.running = false;
                            s.level = 0.0;
                            s.status = "音声処理が終了しました。「デバイスを読み込み直す」を押してください".into();
                        });
                    }
                });
            }
            Err(error) => self.update(|s| {
                s.ready = false;
                s.status = error;
            }),
        }
    }

    fn handle(&self, event: Value) {
        match event["type"].as_str() {
            Some("devices") => {
                let list = |key: &str| -> Vec<Device> {
                    event[key]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|d| Some(Device { id: d["id"].as_u64()? as u32, name: d["name"].as_str().unwrap_or("").to_string() }))
                        .collect()
                };
                let (microphones, gpus) = (list("devices"), list("gpus"));
                self.update(|s| {
                    s.microphones = microphones;
                    s.gpus = gpus;
                    s.ready = true;
                    s.status = "停止中".into();
                });
            }
            Some("status") | Some("error") => {
                let text = event["text"].as_str().unwrap_or("").to_string();
                self.update(|s| s.status = text);
            }
            Some("running") => {
                let running = event["value"].as_bool().unwrap_or(false);
                self.update(|s| s.running = running);
            }
            Some("level") => {
                let level = event["value"].as_f64().unwrap_or(0.0);
                let due = {
                    let mut last = self.last_level.lock().unwrap();
                    let due = last.elapsed() >= Duration::from_millis(100) || level == 0.0;
                    if due {
                        *last = Instant::now();
                    }
                    due
                };
                if due {
                    self.update(|s| s.level = level);
                } else {
                    self.state.lock().unwrap().level = level;
                }
            }
            Some("subtitle") if self.state.lock().unwrap().running => {
                let mask = *self.mask.lock().unwrap();
                let clean = |key: &str| {
                    let text = event[key].as_str().unwrap_or("").to_string();
                    if mask { mask_profanity(&text) } else { text }
                };
                (self.publish)(Event::Subtitle { ja: clean("ja"), en: clean("en") });
            }
            _ => {}
        }
    }

    /// Checks the settings against what the worker found, then starts recognizing.
    pub async fn start(&self, settings: &SubtitleSettings, cuda_ready: bool) -> Result<(), String> {
        let state = self.state();
        if !state.ready {
            return Err("マイクの準備ができていません".into());
        }
        if state.running {
            return Ok(());
        }
        let microphone = settings.microphone.filter(|m| state.microphones.iter().any(|d| d.id == *m)).ok_or("マイクを選択してください")?;
        if settings.device != "cpu" {
            if !cuda_ready {
                return Err("「GPU高速化を導入」を押してください。CPUでも使えます".into());
            }
            if !state.gpus.iter().any(|g| format!("cuda:{}", g.id) == settings.device) {
                return Err("認識に使うデバイスを選択してください".into());
            }
        }
        let command = json!({
            "action": "start",
            "microphone": microphone.to_string(),
            "device": settings.device,
            "model": settings.model,
            "threshold": settings.threshold.to_string(),
        });
        let mut slot = self.worker.lock().await;
        let worker = slot.as_mut().ok_or("マイクの準備ができていません")?;
        worker.send(&command).await?;
        self.update(|s| s.status = "音声認識モデルを準備中".into());
        Ok(())
    }

    pub async fn stop(&self) {
        if let Some(worker) = self.worker.lock().await.as_mut() {
            let _ = worker.send(&json!({"action": "stop"})).await;
        }
    }

    /// Ends the worker at once, without waiting; for app exit.
    pub fn kill(&self) {
        if let Some(pid) = self.pid.lock().unwrap().take() {
            crate::worker::kill_tree(pid);
        }
    }

    /// Ends the worker; `prepare` starts a fresh one.
    pub async fn close(&self) {
        if let Some(worker) = self.worker.lock().await.take() {
            worker.kill();
        }
        self.update(|s| {
            s.ready = false;
            s.running = false;
            s.level = 0.0;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_like_the_electron_version() {
        assert_eq!(mask_profanity("fuck you! FUCK YOU!"), "f**k you! F**K YOU!");
        assert_eq!(
            mask_profanity("fucking fucked fucker fuckers fucks motherfucker"),
            "f**king f**ked f**ker f**kers f**ks motherf**ker"
        );
        assert_eq!(mask_profanity("ファックユー"), "フ＊＊クユー");
        assert_eq!(mask_profanity("good morning, I like sushi. firetruck duck"), "good morning, I like sushi. firetruck duck");
        assert_eq!(mask_profanity("xfuck fuckx"), "xfuck fuckx");
    }

    #[test]
    fn settings_are_kept_valid() {
        let s = SubtitleSettings { device: "cuda:x".into(), model: "huge".into(), threshold: 9.0, ..SubtitleSettings::default() }.sanitized();
        assert_eq!((s.device.as_str(), s.model.as_str(), s.threshold), ("cpu", "large-v3", 0.8));
        assert!(valid_device("cuda:1") && valid_device("cpu") && !valid_device("cuda:") && !valid_device("gpu"));
    }
}
