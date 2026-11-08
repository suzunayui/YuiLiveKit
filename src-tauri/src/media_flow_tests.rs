//! Subtitles and translation end to end with fake workers (`test-fixtures/`) run by the system
//! Python. Skipped when no Python is on PATH.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use tokio::sync::broadcast::Receiver;

use crate::{
    core::{now_ms, Core, Media, Notice},
    db::Db,
    model::{kind, Badges, Batch, ChatMessage, Part, Platform},
    runtime::{Paths, Runtime},
    subtitles::{Launch, SubtitleSettings, Subtitles},
    translation::{Translation, TranslationSettings, MAX_WAITING},
};

fn python() -> Option<PathBuf> {
    let found = std::process::Command::new("python").arg("--version").output().ok()?;
    found.status.success().then(|| PathBuf::from("python"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test-fixtures").join(name)
}

/// A runtime folder that reports subtitles and translation as installed.
fn installed_runtime(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("yuilivekit-media-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for dir in ["python", "llama", "backends/subtitles", "backends/translation"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    for file in ["python/python.exe", "llama/llama-server.exe", "backends/subtitles/worker.py", "backends/translation/worker.py"] {
        std::fs::write(root.join(file), "").unwrap();
    }
    for feature in ["speech", "translation"] {
        std::fs::write(root.join(format!("{feature}.ready.json")), r#"{"version":1}"#).unwrap();
    }
    root
}

fn attach(core: &Arc<Core>, python: PathBuf, root: &PathBuf) -> Arc<Media> {
    let launch = |script: &str| Launch { python: python.clone(), script: fixture(script), cwd: root.clone(), env: HashMap::new() };
    let weak = Arc::downgrade(core);
    let subtitles = Subtitles::new(launch("fake_subtitles.py"), true, move |event| {
        if let Some(core) = weak.upgrade() {
            match event {
                crate::subtitles::Event::State(state) => core.notify_media(Notice::SubtitleState(state)),
                crate::subtitles::Event::Subtitle { ja, en } => core.on_subtitle(ja, en),
            }
        }
    });
    let weak = Arc::downgrade(core);
    let translation = Translation::new(launch("fake_translation.py"), move |event| {
        if let Some(core) = weak.upgrade() {
            core.notify_media(match event {
                crate::translation::Event::State(state) => Notice::TranslationState(state),
                crate::translation::Event::Result { key, ja } => Notice::Translation { key, ja },
                crate::translation::Event::Cleared => Notice::TranslationsCleared,
            });
        }
    });
    let runtime = Runtime::new(Paths { root: root.clone(), source: root.clone() }, |_| {});
    core.attach_media(Media { runtime, subtitles, translation });
    core.media().unwrap()
}

async fn wait<T>(notices: &mut Receiver<Notice>, mut pick: impl FnMut(&Notice) -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let notice = tokio::time::timeout_at(deadline, notices.recv()).await.expect("timed out").expect("notice");
        if let Some(found) = pick(&notice) {
            return found;
        }
    }
}

fn message(id: &str, text: &str) -> ChatMessage {
    ChatMessage {
        platform: Platform::Youtube,
        id: id.into(),
        author_id: Some("UC1".into()),
        author: "A".into(),
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

#[tokio::test(flavor = "multi_thread")]
async fn subtitles_start_with_chat_are_masked_and_stop_with_it() {
    let Some(python) = python() else { return };
    let root = installed_runtime("subtitles");
    let core = Core::new(Db::open_in_memory().unwrap());
    let media = attach(&core, python, &root);
    let mut notices = core.subscribe();
    media.subtitles.prepare().await;
    wait(&mut notices, |n| matches!(n, Notice::SubtitleState(s) if s.ready).then_some(())).await;
    assert_eq!(media.subtitles.state().microphones[0].name, "Mic");

    core.set_subtitle_settings(SubtitleSettings { microphone: Some(1), model: "small".into(), ..SubtitleSettings::default() }).unwrap();
    core.set_translation_settings(TranslationSettings { enabled: false, ..TranslationSettings::default() }).unwrap();
    core.reset_session();
    core.start_media_with_chat();
    let (ja, en) = wait(&mut notices, |n| match n {
        Notice::Subtitle { ja, en } if !ja.is_empty() => Some((ja.clone(), en.clone())),
        _ => None,
    })
    .await;
    assert_eq!((ja.as_str(), en.as_str()), ("フ＊＊クって言った small", "I said f**k"));
    assert!(core.last_subtitle().is_some(), "the overlay can redraw the latest line");

    core.stop();
    wait(&mut notices, |n| matches!(n, Notice::SubtitleState(s) if !s.running && s.status == "停止しました").then_some(())).await;

    core.start_subtitles().await.unwrap();
    wait(&mut notices, |n| matches!(n, Notice::SubtitleState(s) if s.running).then_some(())).await;
    core.stop();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(media.subtitles.state().running, "subtitles started on their own keep running");
    media.subtitles.close().await;
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_microphone_or_gpu_setup_is_explained() {
    let Some(python) = python() else { return };
    let root = installed_runtime("explain");
    let core = Core::new(Db::open_in_memory().unwrap());
    let media = attach(&core, python, &root);
    let mut notices = core.subscribe();
    media.subtitles.prepare().await;
    wait(&mut notices, |n| matches!(n, Notice::SubtitleState(s) if s.ready).then_some(())).await;
    assert!(core.start_subtitles().await.unwrap_err().contains("マイク"));
    core.set_subtitle_settings(SubtitleSettings { microphone: Some(1), device: "cuda:0".into(), ..SubtitleSettings::default() }).unwrap();
    assert!(core.start_subtitles().await.unwrap_err().contains("GPU高速化"));
    media.subtitles.close().await;
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread")]
async fn new_foreign_comments_are_translated_and_cleared_on_stop_k13() {
    let Some(python) = python() else { return };
    let root = installed_runtime("translation");
    let core = Core::new(Db::open_in_memory().unwrap());
    let _media = attach(&core, python, &root);
    let mut notices = core.subscribe();
    core.set_subtitle_settings(SubtitleSettings { start_with_chat: false, ..SubtitleSettings::default() }).unwrap();
    core.set_translation_settings(TranslationSettings { enabled: true, device: "cuda:1".into() }).unwrap();
    core.reset_session();
    core.start_media_with_chat();
    let stream = core.add_stream(Platform::Youtube, "v").unwrap();
    wait(&mut notices, |n| matches!(n, Notice::TranslationState(s) if s.ready).then_some(())).await;

    let mut old = message("old", "Hello from before");
    old.created_ms = now_ms() - 60_000;
    core.ingest(stream, Batch { messages: vec![old, message("ja", "こんにちは"), message("en", "Hello"), message("fail", "fail")], removals: vec![] });
    let (key, ja) = wait(&mut notices, |n| match n {
        Notice::Translation { key, ja } => Some((key.clone(), ja.clone())),
        _ => None,
    })
    .await;
    assert_eq!((key.as_str(), ja.as_str()), ("youtube:en", "訳:Hello"));
    wait(&mut notices, |n| matches!(n, Notice::TranslationState(s) if s.skipped == 1).then_some(())).await;
    assert_eq!(core.translations().len(), 1, "history and Japanese are not translated");

    core.stop();
    wait(&mut notices, |n| matches!(n, Notice::TranslationsCleared).then_some(())).await;
    assert!(core.translations().is_empty(), "translations leave the screen when stopped");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_waiting_queue_drops_the_oldest_k13() {
    let launch = Launch { python: "missing".into(), script: "missing".into(), cwd: std::env::temp_dir(), env: HashMap::new() };
    let translation = Translation::new(launch, |_| {});
    // Mark it active without a worker, to look at the queue alone.
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        translation.start("cpu");
        for i in 0..(MAX_WAITING + 5) {
            translation.enqueue(&format!("k{i}"), "Hello", kind::TEXT);
        }
        translation.enqueue("k0", "Hello", kind::TEXT);
        let state = translation.state();
        assert!(state.queued <= MAX_WAITING, "{state:?}");
        assert!(state.skipped >= 5);
    });
}
