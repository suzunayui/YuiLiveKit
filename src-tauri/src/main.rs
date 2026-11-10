// Tauri glue: the window, its commands, and the local server for OBS.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::Serialize;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};
use yui_live_kit::{
    core::{Connection, Core, Media, Notice, Status, ToolSettings},
    db::{Db, ImportSummary, Shown},
    appearance::{self, Appearance},
    background::{self, Action, ObsLink},
    obs_plugin::{self, PluginStatus},
    runtime::{Feature, Paths, Runtime, RuntimeState},
    subtitles::{Launch, SubtitleSettings, SubtitleState, Subtitles},
    translation::{Translation, TranslationSettings, TranslationState},
    speech::{self, AiSettings, Speaker, SpeechSettings, Viewers, Voice},
    voicevox::{self, VoicevoxState},
    tools::{ClearMode, Emoji, Ohayo, Participant, Support},
};

const DEFAULT_PORT: u16 = 5000;
const CANVAS: (u32, u32) = (1920, 1080);
const LIST_SIZE: usize = 300;
/// Built by obs-plugin/build.cmd and copied to obs-plugin/prebuilt, so the app can install it.
const OBS_PLUGIN: &[u8] = include_bytes!("../../obs-plugin/prebuilt/yui-live-kit.dll");

type CoreState<'a> = tauri::State<'a, Arc<Core>>;

fn port() -> u16 {
    std::env::var("YUI_LIVE_KIT_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_PORT)
}

fn part_url(port: u16, id: &str) -> String {
    format!("http://127.0.0.1:{port}/overlay/{id}")
}

#[derive(Serialize)]
struct PartSource {
    id: &'static str,
    label: &'static str,
    source: &'static str,
    url: String,
    drag_url: String,
    width: u32,
    height: u32,
}

fn part_sources(port: u16) -> Vec<PartSource> {
    appearance::PARTS
        .iter()
        .map(|p| {
            let url = part_url(port, p.id);
            PartSource { id: p.id, label: p.label, source: p.source, drag_url: appearance::drag_url(&url, p), url, width: p.width, height: p.height }
        })
        .collect()
}

/// The OBS plugin reads this file to learn which sources to add, back to front.
fn write_obs_source_file(dir: &Path, port: u16) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let sources: Vec<serde_json::Value> = appearance::PARTS
        .iter()
        .map(|p| {
            serde_json::json!({"name": p.source, "url": part_url(port, p.id), "width": p.width, "height": p.height, "x": p.x, "y": p.y})
        })
        .collect();
    let file = serde_json::json!({"canvas_width": CANVAS.0, "canvas_height": CANVAS.1, "sources": sources});
    std::fs::write(dir.join("obs-source.json"), serde_json::to_vec_pretty(&file)?)
}

/// OBS wants the app kept running: closing the window hides it, and OBS closing ends it.
struct KeptForObs(std::sync::atomic::AtomicBool);

/// Tells the OBS plugin where the app is and whether to start it with OBS. Kept apart from
/// obs-source.json, whose every change makes the plugin reload the overlay sources.
fn write_obs_plugin_file(dir: &Path, link: ObsLink) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let app = std::env::current_exe()?;
    let file = serde_json::json!({"app": app.display().to_string(), "start_with_obs": link.start_with_obs});
    std::fs::write(dir.join("obs-plugin.json"), serde_json::to_vec_pretty(&file)?)
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[tauri::command]
fn set_obs_link(app: tauri::AppHandle, core: CoreState<'_>, link: ObsLink) -> Result<ObsLink, String> {
    let link = core.set_obs_link(link)?;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    write_obs_plugin_file(&dir, link).map_err(|e| format!("OBS プラグイン向けの設定を書けませんでした: {e}"))?;
    Ok(link)
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
    parts: Vec<PartSource>,
    server_error: Option<String>,
    statuses: std::collections::BTreeMap<&'static str, Status>,
    messages: Vec<Shown>,
    participants: Vec<Participant>,
    ohayo: Ohayo,
    support: Vec<Support>,
    tool_settings: ToolSettings,
    connection: Connection,
    obs_link: ObsLink,
    appearance: Appearance,
    legacy_imported: bool,
    legacy_candidates: Vec<String>,
}

#[tauri::command]
fn get_state(core: CoreState<'_>, error: tauri::State<'_, ServerError>) -> AppState {
    AppState {
        parts: part_sources(port()),
        server_error: error.0.lock().unwrap().clone(),
        statuses: core.statuses(),
        messages: core.snapshot(LIST_SIZE),
        participants: core.participants(),
        ohayo: core.ohayo(),
        support: core.support(),
        tool_settings: core.tool_settings(),
        connection: core.connection(),
        obs_link: core.obs_link(),
        appearance: core.appearance(),
        legacy_imported: core.db().legacy_imported().unwrap_or(false),
        legacy_candidates: legacy_candidates(),
    }
}

#[tauri::command]
async fn start_chat(core: CoreState<'_>, youtube: String, twitch: String) -> Result<(), String> {
    core.start(&youtube, &twitch)
}

#[tauri::command]
fn stop_chat(core: CoreState<'_>) {
    core.stop();
}

#[tauri::command]
async fn import_legacy(core: CoreState<'_>, path: String) -> Result<ImportSummary, String> {
    if !legacy_candidates().contains(&path) {
        return Err("取り込めるのは、見つかった以前のデータベースだけです".into());
    }
    let core = core.inner().clone();
    tauri::async_runtime::spawn_blocking(move || core.db().import_legacy(Path::new(&path)))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn set_participant_status(core: CoreState<'_>, id: i64, status: String) -> Result<Vec<Participant>, String> {
    core.set_participant_status(id, &status)
}

#[tauri::command]
fn clear_participants(core: CoreState<'_>, mode: ClearMode) -> Result<Vec<Participant>, String> {
    core.clear_participants(mode)
}

#[tauri::command]
fn reorder_participants(core: CoreState<'_>, ids: Vec<i64>) -> Result<Vec<Participant>, String> {
    core.reorder_participants(&ids)
}

#[tauri::command]
fn adjust_ohayo(core: CoreState<'_>, delta: i64) -> Result<Ohayo, String> {
    core.adjust_ohayo(delta.clamp(-1000, 1000))
}

#[tauri::command]
fn reset_ohayo(core: CoreState<'_>) -> Result<Ohayo, String> {
    core.reset_ohayo()
}

#[tauri::command]
fn set_tool_settings(
    core: CoreState<'_>,
    participant_keywords: Vec<String>,
    withdraw_keywords: Vec<String>,
    ohayo_keywords: Vec<String>,
) -> Result<ToolSettings, String> {
    core.set_tool_settings(&participant_keywords, &withdraw_keywords, &ohayo_keywords)
}

#[tauri::command]
fn set_support_read(core: CoreState<'_>, ids: Vec<i64>, read: bool) -> Result<Vec<Support>, String> {
    core.set_support_read(&ids, read)
}

#[tauri::command]
fn save_appearance(core: CoreState<'_>, appearance: Appearance) -> Result<Appearance, String> {
    core.save_appearance(appearance)
}

#[tauri::command]
async fn obs_plugin_status() -> PluginStatus {
    tauri::async_runtime::spawn_blocking(|| obs_plugin::status(OBS_PLUGIN)).await.unwrap_or(PluginStatus {
        state: obs_plugin::PluginState::Missing,
        path: String::new(),
        obs_running: false,
    })
}

#[tauri::command]
async fn install_obs_plugin(app: tauri::AppHandle) -> Result<PluginStatus, String> {
    let scratch = app.path().app_local_data_dir().map_err(|e| e.to_string())?.join("obs-plugin");
    tauri::async_runtime::spawn_blocking(move || obs_plugin::install(OBS_PLUGIN, &scratch)).await.map_err(|e| e.to_string())?
}

#[derive(Serialize)]
struct VoiceState {
    speech: SpeechSettings,
    ai: AiSettings,
    viewers: Viewers,
    has_api_key: bool,
    emoji: Vec<Emoji>,
}

fn voice_state(core: &Core) -> VoiceState {
    VoiceState {
        speech: core.speech_settings(),
        ai: core.ai_settings(),
        viewers: core.viewers(),
        has_api_key: core.has_api_key(),
        emoji: core.db().emoji().unwrap_or_default(),
    }
}

#[tauri::command]
fn get_voice_state(core: CoreState<'_>) -> VoiceState {
    voice_state(&core)
}

#[tauri::command]
async fn voicevox_state(core: CoreState<'_>) -> Result<VoicevoxState, String> {
    Ok(core.voicevox().ok_or("VOICEVOX は使えません")?.refresh().await)
}

#[tauri::command]
async fn voicevox_icons(core: CoreState<'_>) -> Result<std::collections::BTreeMap<u32, String>, String> {
    core.voicevox_icons().await
}

#[tauri::command]
async fn uninstall_voicevox(core: CoreState<'_>) -> Result<(), String> {
    core.voicevox().ok_or("VOICEVOX は使えません")?.uninstall().await
}

#[tauri::command]
async fn install_voicevox(core: CoreState<'_>) -> Result<(), String> {
    core.voicevox().ok_or("VOICEVOX は使えません")?.install().await
}

#[tauri::command]
fn set_speech_settings(core: CoreState<'_>, settings: SpeechSettings) -> Result<SpeechSettings, String> {
    core.set_speech_settings(settings)
}

#[tauri::command]
fn set_ai_settings(core: CoreState<'_>, settings: AiSettings) -> Result<AiSettings, String> {
    core.set_ai_settings(settings)
}

/// The key goes straight to Windows Credential Manager and is never sent back to the screen.
#[tauri::command]
fn set_api_key(core: CoreState<'_>, key: Option<String>) -> Result<bool, String> {
    core.set_api_key(key.as_deref())
}

#[tauri::command]
async fn voicevox_speakers(core: CoreState<'_>) -> Result<Vec<Speaker>, String> {
    core.voicevox_speakers().await
}

#[tauri::command]
fn speak_test(core: CoreState<'_>, text: String, voice: Voice) {
    core.speak_test(&text, voice);
}

#[tauri::command]
fn set_viewer_name(core: CoreState<'_>, viewer: String, reading: String) -> Result<Viewers, String> {
    core.set_viewer_name(&viewer, &reading)
}

#[tauri::command]
fn set_blocked(core: CoreState<'_>, viewer: String, blocked: bool) -> Result<Viewers, String> {
    core.set_blocked(&viewer, blocked)
}

#[tauri::command]
fn set_emoji(core: CoreState<'_>, code: String, reading: Option<String>, volume: Option<f64>, clear_sound: bool) -> Result<Vec<Emoji>, String> {
    let db = core.db();
    db.update_emoji(&code, reading.as_deref(), clear_sound.then_some(None), volume)?;
    db.emoji()
}

const SOUND_EXTENSIONS: [&str; 4] = ["wav", "mp3", "ogg", "m4a"];
const MAX_SOUND_BYTES: u64 = 10 * 1024 * 1024;

/// Copies a chosen sound into the app's folder, so moving or deleting the original does not break it.
#[tauri::command]
async fn choose_emoji_sound(app: tauri::AppHandle, core: CoreState<'_>, code: String) -> Result<Vec<Emoji>, String> {
    use tauri_plugin_dialog::DialogExt;
    let picker = app.dialog().file().set_title("効果音を選ぶ").add_filter("音声", &SOUND_EXTENSIONS);
    let Some(picked) = tauri::async_runtime::spawn_blocking(move || picker.blocking_pick_file()).await.map_err(|e| e.to_string())? else {
        return core.db().emoji();
    };
    let source = picked.into_path().map_err(|e| e.to_string())?;
    let extension = source.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    if !SOUND_EXTENSIONS.contains(&extension.as_str()) {
        return Err("wav、mp3、ogg、m4a のファイルを選んでください".into());
    }
    if std::fs::metadata(&source).map_err(|e| e.to_string())?.len() > MAX_SOUND_BYTES {
        return Err("効果音は10MBまでです".into());
    }
    let bytes = std::fs::read(&source).map_err(|e| e.to_string())?;
    let name = {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(&bytes);
        format!("{}.{extension}", digest.iter().take(8).map(|b| format!("{b:02x}")).collect::<String>())
    };
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("sounds");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join(name);
    std::fs::write(&target, &bytes).map_err(|e| e.to_string())?;
    let db = core.db();
    db.update_emoji(&code, None, Some(Some(&target.display().to_string())), None)?;
    db.emoji()
}

#[derive(Serialize)]
struct MediaState {
    subtitle_settings: SubtitleSettings,
    translation_settings: TranslationSettings,
    subtitles: SubtitleState,
    translation: TranslationState,
    runtime: RuntimeState,
    gpus: Vec<yui_live_kit::subtitles::Device>,
}

/// NVIDIA GPUs for translation (subtitles get theirs from the worker).
fn nvidia_gpus() -> Vec<yui_live_kit::subtitles::Device> {
    let mut command = std::process::Command::new("nvidia-smi");
    command.args(["--query-gpu=index,name", "--format=csv,noheader"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| {
                    let (index, name) = line.split_once(',')?;
                    Some(yui_live_kit::subtitles::Device { id: index.trim().parse().ok()?, name: name.trim().to_string() })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[tauri::command]
async fn get_media_state(core: CoreState<'_>) -> Result<MediaState, String> {
    let media = core.media().ok_or("音声字幕とコメント翻訳は使えません")?;
    let (subtitle_settings, translation_settings) = core.media_settings();
    let gpus = tauri::async_runtime::spawn_blocking(nvidia_gpus).await.unwrap_or_default();
    Ok(MediaState {
        subtitle_settings,
        translation_settings,
        subtitles: media.subtitles.state(),
        translation: media.translation.state(),
        runtime: media.runtime.state(),
        gpus,
    })
}

#[tauri::command]
fn set_subtitle_settings(core: CoreState<'_>, settings: SubtitleSettings) -> Result<SubtitleSettings, String> {
    core.set_subtitle_settings(settings)
}

#[tauri::command]
fn set_translation_settings(core: CoreState<'_>, settings: TranslationSettings) -> Result<TranslationSettings, String> {
    core.set_translation_settings(settings)
}

#[tauri::command]
async fn start_subtitles(core: CoreState<'_>) -> Result<(), String> {
    core.start_subtitles().await
}

#[tauri::command]
async fn stop_subtitles(core: CoreState<'_>) -> Result<(), String> {
    core.stop_subtitles().await;
    Ok(())
}

#[tauri::command]
async fn reload_devices(core: CoreState<'_>) -> Result<(), String> {
    core.reload_devices().await;
    Ok(())
}

#[tauri::command]
fn clear_subtitle(core: CoreState<'_>) {
    core.clear_subtitle();
}

#[tauri::command]
async fn install_feature(core: CoreState<'_>, feature: Feature) -> Result<(), String> {
    core.install_feature(feature).await
}

/// Python files shipped with the app; next to the sources when running a development build.
fn python_source(app: &tauri::AppHandle) -> PathBuf {
    let bundled = app.path().resource_dir().map(|dir| dir.join("python")).ok().filter(|dir| dir.join("subtitles").exists());
    bundled.unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("python"))
}

fn attach_media(app: &tauri::AppHandle, core: &Arc<Core>, data_dir: &Path) {
    let root = std::env::var_os("YUI_LIVE_KIT_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| data_dir.join("optional-runtime"));
    let models = std::env::var_os("YUI_LIVE_KIT_MODEL_DIR").map(PathBuf::from).unwrap_or_else(|| data_dir.join("models"));
    let paths = Paths { root: root.clone(), source: python_source(app) };
    let weak = Arc::downgrade(core);
    let runtime = Runtime::new(paths.clone(), move |state| {
        if let Some(core) = weak.upgrade() {
            core.notify_media(Notice::Runtime(state));
        }
    });
    let env = |extra: &[(&str, PathBuf)]| -> std::collections::HashMap<String, String> {
        let mut env = std::collections::HashMap::from([("SUBTITLE_MODEL_DIR".to_string(), models.display().to_string())]);
        for (key, value) in extra {
            env.insert(key.to_string(), value.display().to_string());
        }
        env
    };
    let speech_launch =
        Launch { python: paths.python(), script: paths.worker(Feature::Speech), cwd: data_dir.to_path_buf(), env: env(&[]) };
    let translation_launch = Launch {
        python: paths.python(),
        script: paths.worker(Feature::Translation),
        cwd: data_dir.to_path_buf(),
        env: env(&[("SUBTITLE_TRANSLATION_RUNTIME", paths.llama())]),
    };
    let weak = Arc::downgrade(core);
    let subtitles = Subtitles::new(speech_launch, core.media_settings().0.mask_profanity, move |event| {
        if let Some(core) = weak.upgrade() {
            match event {
                yui_live_kit::subtitles::Event::State(state) => core.notify_media(Notice::SubtitleState(state)),
                yui_live_kit::subtitles::Event::Subtitle { ja, en } => core.on_subtitle(ja, en),
            }
        }
    });
    let weak = Arc::downgrade(core);
    let translation = Translation::new(translation_launch, move |event| {
        if let Some(core) = weak.upgrade() {
            core.notify_media(match event {
                yui_live_kit::translation::Event::State(state) => Notice::TranslationState(state),
                yui_live_kit::translation::Event::Result { key, ja } => Notice::Translation { key, ja },
                yui_live_kit::translation::Event::Cleared => Notice::TranslationsCleared,
            });
        }
    });
    let speech_ready = runtime.ready(Feature::Speech);
    core.attach_media(Media { runtime, subtitles: subtitles.clone(), translation });
    if speech_ready {
        tauri::async_runtime::spawn(async move { subtitles.prepare().await });
    }
}

fn main() {
    let port = port();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // The app is running and was launched again: by the user, or by OBS starting or closing.
            use std::sync::atomic::Ordering;
            let kept = app.state::<KeptForObs>();
            let visible = app.get_webview_window("main").and_then(|w| w.is_visible().ok()).unwrap_or(false);
            let (action, keep) = background::on_second_launch(background::launch_kind(&args), kept.0.load(Ordering::SeqCst), visible);
            kept.0.store(keep, Ordering::SeqCst);
            match action {
                Action::ShowWindow => show_main_window(app),
                Action::Exit => app.exit(0),
                Action::Nothing => {}
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .manage(ServerError(Mutex::new(None)))
        .manage(KeptForObs(std::sync::atomic::AtomicBool::new(false)))
        .on_window_event(|window, event| {
            // While OBS wants the app kept running, closing the window only puts it away.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let kept = window.state::<KeptForObs>().0.load(std::sync::atomic::Ordering::SeqCst);
                if window.label() == "main" && background::hide_on_close(kept) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            start_chat,
            stop_chat,
            import_legacy,
            set_participant_status,
            clear_participants,
            reorder_participants,
            adjust_ohayo,
            reset_ohayo,
            set_tool_settings,
            set_obs_link,
            set_support_read,
            save_appearance,
            obs_plugin_status,
            install_obs_plugin,
            get_voice_state,
            get_media_state,
            set_subtitle_settings,
            set_translation_settings,
            start_subtitles,
            stop_subtitles,
            reload_devices,
            clear_subtitle,
            install_feature,
            set_speech_settings,
            set_ai_settings,
            set_api_key,
            voicevox_speakers,
            voicevox_state,
            voicevox_icons,
            install_voicevox,
            uninstall_voicevox,
            speak_test,
            set_viewer_name,
            set_blocked,
            set_emoji,
            choose_emoji_sound,
        ])
        .setup(move |app| {
            let args: Vec<String> = std::env::args().collect();
            let launch = background::launch_kind(&args);
            if launch == background::Launch::QuitBackground {
                // OBS closed and the app was not running: there is nothing to close.
                app.handle().exit(0);
                return Ok(());
            }
            app.state::<KeptForObs>().0.store(launch == background::Launch::Background, std::sync::atomic::Ordering::SeqCst);
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title(&format!("YuiLiveKit {}", app.package_info().version));
            }
            // The window starts hidden; OBS starting the app leaves it that way.
            if launch == background::Launch::Normal {
                show_main_window(app.handle());
            }

            // The tray icon is the way in while the window is put away.
            let open = MenuItem::with_id(app, "open", "YuiLiveKit を開く", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "終了", true, None::<&str>)?;
            let mut tray = TrayIconBuilder::with_id("main")
                .tooltip("YuiLiveKit")
                .menu(&Menu::with_items(app, &[&open, &quit])?)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => show_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        show_main_window(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            let data_dir = app.path().app_data_dir()?;
            let core = Core::new(Db::open(&data_dir.join("livekit.db"))?);
            app.manage(core.clone());
            if let Err(error) = write_obs_plugin_file(&data_dir, core.obs_link()) {
                eprintln!("could not write obs-plugin.json: {error}");
            }
            // Started by OBS, with the user's say-so: collect from where chat was last collected.
            let connection = core.connection();
            let somewhere = !connection.youtube.is_empty() || !connection.twitch.is_empty();
            if background::should_auto_start(launch, core.obs_link(), somewhere) {
                let core = core.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = core.start(&connection.youtube, &connection.twitch) {
                        eprintln!("could not start collecting for OBS: {error}");
                    }
                });
            }
            tauri::async_runtime::spawn(core.clone().run_speech());
            attach_media(app.handle(), &core, &data_dir);
            let voicevox_dir =
                std::env::var_os("YUI_LIVE_KIT_VOICEVOX_DIR").map(PathBuf::from).unwrap_or_else(|| data_dir.join("voicevox").join("engine"));
            let weak = Arc::downgrade(&core);
            core.attach_voicevox(voicevox::Engine::new(voicevox_dir, speech::VOICEVOX_BASE, move |state| {
                if let Some(core) = weak.upgrade() {
                    core.notify_media(Notice::Voicevox(state));
                }
            }));

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
                    // The OBS plugin reloads its sources when this file changes, so it is
                    // written only once the overlay pages can be loaded.
                    if let Err(error) = write_obs_source_file(&data_dir, port) {
                        eprintln!("could not write obs-source.json: {error}");
                    }
                    axum::serve(listener, yui_live_kit::server::router(core, port)).await
                }
                .await;
                if let Err(error) = result {
                    let message = format!(
                        "ポート{port}を使えないため、OBSに表示できません。ほかのアプリが使っていないか確認してください（{error}）"
                    );
                    eprintln!("{message}");
                    *handle.state::<ServerError>().0.lock().unwrap() = Some(message.clone());
                    let _ = handle.emit("server-error", message);
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building YuiLiveKit")
        .run(|app, event| {
            // Python, faster-whisper and llama.cpp must not outlive the app.
            if let tauri::RunEvent::Exit = event {
                if let Some(core) = app.try_state::<Arc<Core>>() {
                    core.shutdown();
                }
            }
        });
}
