// Tauri glue: the window, its commands, and the local server for OBS.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::Serialize;
use tauri::{Emitter, Manager};
use yui_live_kit::{
    core::{Core, Status, ToolSettings},
    db::{Db, ImportSummary, Shown},
    layout::{Layout, Layouts},
    obs_plugin::{self, PluginStatus},
    speech::{AiSettings, Speaker, SpeechSettings, Viewers, Voice},
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

fn overlay_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/overlay")
}

/// The OBS plugin reads this file to learn where the overlay is.
fn write_obs_source_file(dir: &Path, port: u16) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let source = serde_json::json!({"url": overlay_url(port), "width": CANVAS.0, "height": CANVAS.1});
    std::fs::write(dir.join("obs-source.json"), serde_json::to_vec_pretty(&source)?)
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
    overlay_url: String,
    server_error: Option<String>,
    statuses: std::collections::BTreeMap<&'static str, Status>,
    messages: Vec<Shown>,
    participants: Vec<Participant>,
    ohayo: Ohayo,
    support: Vec<Support>,
    tool_settings: ToolSettings,
    layouts: Layouts,
    legacy_imported: bool,
    legacy_candidates: Vec<String>,
}

#[tauri::command]
fn get_state(core: CoreState<'_>, error: tauri::State<'_, ServerError>) -> AppState {
    AppState {
        overlay_url: overlay_url(port()),
        server_error: error.0.lock().unwrap().clone(),
        statuses: core.statuses(),
        messages: core.snapshot(LIST_SIZE),
        participants: core.participants(),
        ohayo: core.ohayo(),
        support: core.support(),
        tool_settings: core.tool_settings(),
        layouts: core.layouts(),
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
fn set_tool_settings(core: CoreState<'_>, participant_keyword: String, ohayo_keywords: Vec<String>) -> Result<ToolSettings, String> {
    core.set_tool_settings(&participant_keyword, &ohayo_keywords)
}

#[tauri::command]
fn set_support_read(core: CoreState<'_>, ids: Vec<i64>, read: bool) -> Result<Vec<Support>, String> {
    core.set_support_read(&ids, read)
}

#[tauri::command]
fn save_layout(core: CoreState<'_>, name: String, layout: Layout) -> Result<Layouts, String> {
    core.save_layout(&name, layout)
}

#[tauri::command]
fn delete_layout(core: CoreState<'_>, name: String) -> Result<Layouts, String> {
    core.delete_layout(&name)
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

fn main() {
    let port = port();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .manage(ServerError(Mutex::new(None)))
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
            set_support_read,
            save_layout,
            delete_layout,
            obs_plugin_status,
            install_obs_plugin,
            get_voice_state,
            set_speech_settings,
            set_ai_settings,
            set_api_key,
            voicevox_speakers,
            speak_test,
            set_viewer_name,
            set_blocked,
            set_emoji,
            choose_emoji_sound,
        ])
        .setup(move |app| {
            let data_dir = app.path().app_data_dir()?;
            if let Err(error) = write_obs_source_file(&data_dir, port) {
                eprintln!("could not write obs-source.json: {error}");
            }
            let core = Core::new(Db::open(&data_dir.join("livekit.db"))?);
            app.manage(core.clone());
            tauri::async_runtime::spawn(core.clone().run_speech());

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
        .run(tauri::generate_context!())
        .expect("error while running YuiLiveKit");
}
