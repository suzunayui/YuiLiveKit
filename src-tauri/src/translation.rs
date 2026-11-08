//! Japanese translations of foreign comments with TranslateGemma on llama.cpp
//! (`python/translation/worker.py`). Translations stay in memory and never delay the comment.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{model::kind, subtitles::Launch, worker::Worker};

pub const MAX_WAITING: usize = 30;
const MAX_RESULTS: usize = 1000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TranslationSettings {
    pub enabled: bool,
    pub device: String,
}

impl Default for TranslationSettings {
    fn default() -> Self {
        Self { enabled: true, device: "cpu".into() }
    }
}

impl TranslationSettings {
    pub fn sanitized(mut self) -> Self {
        if !crate::subtitles::valid_device(&self.device) {
            self.device = "cpu".into();
        }
        self
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct TranslationState {
    pub active: bool,
    pub ready: bool,
    pub status: String,
    pub translated: u64,
    pub skipped: u64,
    pub queued: usize,
}

pub enum Event {
    State(TranslationState),
    Result { key: String, ja: String },
    Cleared,
}

/// Worth translating: has letters, and no kana (Japanese is left alone). URLs are ignored.
pub fn candidate(text: &str) -> bool {
    let urls = regex::Regex::new(r"https?://\S+").expect("pattern");
    let clean = urls.replace_all(text, "");
    let clean = clean.trim();
    clean.chars().any(char::is_alphabetic) && !clean.chars().any(|c| ('\u{3040}'..='\u{30ff}').contains(&c))
}

struct Job {
    key: String,
    text: String,
}

pub struct Translation {
    launch: Launch,
    state: Mutex<TranslationState>,
    queue: Mutex<VecDeque<Job>>,
    seen: Mutex<HashSet<String>>,
    results: Mutex<(HashMap<String, String>, VecDeque<String>)>,
    wake: tokio::sync::Notify,
    generation: Mutex<u64>,
    /// The running worker, so stopping can end it (and llama.cpp) at once.
    pid: Mutex<Option<u32>>,
    publish: Arc<dyn Fn(Event) + Send + Sync>,
}

impl Translation {
    pub fn new(launch: Launch, publish: impl Fn(Event) + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            launch,
            state: Mutex::new(TranslationState { status: "翻訳は停止中です".into(), ..TranslationState::default() }),
            queue: Mutex::new(VecDeque::new()),
            seen: Mutex::new(HashSet::new()),
            results: Mutex::new((HashMap::new(), VecDeque::new())),
            wake: tokio::sync::Notify::new(),
            generation: Mutex::new(0),
            pid: Mutex::new(None),
            publish: Arc::new(publish),
        })
    }

    pub fn state(&self) -> TranslationState {
        self.state.lock().unwrap().clone()
    }

    fn update(&self, change: impl FnOnce(&mut TranslationState)) {
        let snapshot = {
            let mut state = self.state.lock().unwrap();
            change(&mut state);
            state.queued = self.queue.lock().unwrap().len();
            state.clone()
        };
        (self.publish)(Event::State(snapshot));
    }

    /// Translations made so far, for redrawing the overlay and the list.
    pub fn results(&self) -> HashMap<String, String> {
        self.results.lock().unwrap().0.clone()
    }

    /// Queues a new comment. Japanese, emoji-only and other kinds are skipped; when more than
    /// 30 wait, the oldest is dropped (K13).
    pub fn enqueue(&self, key: &str, text: &str, message_kind: &str) {
        if !self.state.lock().unwrap().active || !self.seen.lock().unwrap().insert(key.to_string()) {
            return;
        }
        if ![kind::TEXT, kind::PAID, kind::MEMBERSHIP].contains(&message_kind) || !candidate(text) {
            return;
        }
        let dropped = {
            let mut queue = self.queue.lock().unwrap();
            queue.push_back(Job { key: key.to_string(), text: text.chars().take(2000).collect() });
            if queue.len() > MAX_WAITING {
                queue.pop_front();
                true
            } else {
                false
            }
        };
        self.update(|s| s.skipped += u64::from(dropped));
        self.wake.notify_one();
    }

    fn remember(&self, key: String, ja: String) {
        let mut results = self.results.lock().unwrap();
        if results.0.insert(key.clone(), ja).is_none() {
            results.1.push_back(key);
        }
        while results.1.len() > MAX_RESULTS {
            if let Some(old) = results.1.pop_front() {
                results.0.remove(&old);
            }
        }
    }

    /// Starts the worker on the given device and translates queued comments one at a time.
    pub fn start(self: &Arc<Self>, device: &str) {
        let generation = {
            let mut g = self.generation.lock().unwrap();
            *g += 1;
            *g
        };
        self.queue.lock().unwrap().clear();
        self.seen.lock().unwrap().clear();
        self.update(|s| {
            *s = TranslationState { active: true, status: "TranslateGemma 12Bを準備中".into(), ..TranslationState::default() };
        });
        let me = self.clone();
        let device = device.to_string();
        tokio::spawn(async move {
            if let Err(error) = me.run(generation, &device).await {
                if *me.generation.lock().unwrap() == generation {
                    me.update(|s| {
                        s.active = false;
                        s.ready = false;
                        s.status = format!("翻訳エラー: {error}");
                    });
                }
            }
        });
    }

    fn current(&self, generation: u64) -> bool {
        *self.generation.lock().unwrap() == generation
    }

    async fn run(&self, generation: u64, device: &str) -> Result<(), String> {
        let args = vec!["-u".to_string(), self.launch.script.display().to_string()];
        let (mut worker, mut events) = Worker::spawn(&self.launch.python, &args, &self.launch.env, &self.launch.cwd)?;
        *self.pid.lock().unwrap() = worker.pid();
        if !self.current(generation) {
            worker.kill();
            return Ok(());
        }
        let result = async {
            worker.send(&json!({"device": device})).await?;
            loop {
                let event = events.recv().await.ok_or("翻訳処理が終了しました")?;
                if !self.current(generation) {
                    return Ok(());
                }
                match event["type"].as_str() {
                    Some("status") => {
                        let text = event["text"].as_str().unwrap_or("").to_string();
                        self.update(|s| s.status = text);
                    }
                    Some("ready") => break,
                    Some("error") => return Err(event["text"].as_str().unwrap_or("翻訳処理のエラー").to_string()),
                    _ => {}
                }
            }
            self.update(|s| {
                s.ready = true;
                s.status = "TranslateGemma 12Bで翻訳中".into();
            });
            let mut next_id: u64 = 0;
            while self.current(generation) {
                let job = self.queue.lock().unwrap().pop_front();
                let Some(job) = job else {
                    tokio::select! {
                        _ = self.wake.notified() => {}
                        _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                    }
                    continue;
                };
                next_id += 1;
                worker.send(&json!({"id": next_id, "text": job.text})).await?;
                let answer: Value = loop {
                    let event = events.recv().await.ok_or("翻訳処理が終了しました")?;
                    if event["id"].as_u64() == Some(next_id) {
                        break event;
                    }
                };
                if !self.current(generation) {
                    break;
                }
                let ja = answer["ja"].as_str().unwrap_or("").trim().to_string();
                let language = answer["language"].as_str().unwrap_or("");
                if answer["type"] == "result" && language != "ja" && !ja.is_empty() && ja != job.text.trim() {
                    self.remember(job.key.clone(), ja.clone());
                    (self.publish)(Event::Result { key: job.key, ja });
                    self.update(|s| s.translated += 1);
                } else if answer["type"] == "translation-error" {
                    self.update(|s| {
                        s.skipped += 1;
                        s.status = "コメントを翻訳できませんでした".into();
                    });
                } else {
                    self.update(|_| {});
                }
            }
            Ok::<(), String>(())
        }
        .await;
        worker.kill();
        result
    }

    /// Stops the worker and removes the translations from the screens.
    pub fn stop(&self) {
        *self.generation.lock().unwrap() += 1;
        if let Some(pid) = self.pid.lock().unwrap().take() {
            crate::worker::kill_tree(pid);
        }
        self.queue.lock().unwrap().clear();
        {
            let mut results = self.results.lock().unwrap();
            results.0.clear();
            results.1.clear();
        }
        self.wake.notify_one();
        self.update(|s| {
            s.active = false;
            s.ready = false;
            s.status = "翻訳は停止中です".into();
        });
        (self.publish)(Event::Cleared);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_foreign_text_is_translated() {
        assert!(candidate("Hello everyone"));
        assert!(candidate("你好"), "kanji without kana may be Chinese");
        assert!(candidate("Привет"));
        assert!(!candidate("こんにちは"));
        assert!(!candidate("草wwwこれは"));
        assert!(!candidate("😀👍"));
        assert!(!candidate("https://example.com/hello"));
        assert!(!candidate("1234 !!"));
    }
}
