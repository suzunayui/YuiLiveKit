//! The VOICEVOX engine: installs the official CPU build when asked, and runs it while the app
//! needs it. A VOICEVOX the user already runs on the same port is used as it is.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::Serialize;

use crate::runtime::{download_checked, Asset};

pub const VERSION: &str = "0.26.1";
/// The CPU build: a `.vvpp`, which is a zip. Checked against the SHA-256 GitHub lists for it.
const ENGINE: Asset = Asset {
    url: "https://github.com/VOICEVOX/voicevox_engine/releases/download/0.26.1/voicevox_engine-windows-cpu-0.26.1.vvpp",
    sha256: "f4a7a0261eb7ed4a21e461e012734f9be3cff6d15b4a2e14983c791799dacf54",
};
/// The engine loads its voices before it answers; on a slow PC that takes a while.
const START_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct VoicevoxState {
    /// The engine this app installed is there.
    pub installed: bool,
    /// Something answers on the VOICEVOX port (this app's engine or the user's own VOICEVOX).
    pub running: bool,
    pub busy: bool,
    pub status: String,
}

pub struct Engine {
    dir: PathBuf,
    base: String,
    state: Mutex<VoicevoxState>,
    /// The engine this app started, shared with the thread that waits for it to exit.
    child: Arc<Mutex<Option<u32>>>,
    /// One start at a time; a second caller waits for the first.
    starting: tokio::sync::Mutex<()>,
    http: reqwest::Client,
    publish: Box<dyn Fn(VoicevoxState) + Send + Sync>,
}

fn find_run_exe(dir: &Path, depth: usize) -> Option<PathBuf> {
    if dir.join("run.exe").is_file() {
        return Some(dir.to_path_buf());
    }
    if depth == 0 {
        return None;
    }
    std::fs::read_dir(dir).ok()?.flatten().filter(|e| e.path().is_dir()).find_map(|e| find_run_exe(&e.path(), depth - 1))
}

impl Engine {
    /// `dir` is where the engine goes; `base` is the VOICEVOX address the app talks to.
    pub fn new(dir: PathBuf, base: &str, publish: impl Fn(VoicevoxState) + Send + Sync + 'static) -> Arc<Self> {
        let engine = Arc::new(Self {
            dir,
            base: base.trim_end_matches('/').to_string(),
            state: Mutex::new(VoicevoxState::default()),
            child: Arc::new(Mutex::new(None)),
            starting: tokio::sync::Mutex::new(()),
            http: reqwest::Client::builder().timeout(Duration::from_secs(3)).build().unwrap_or_default(),
            publish: Box::new(publish),
        });
        let installed = engine.installed();
        engine.update(|s| {
            s.installed = installed;
            s.status = if installed { "VOICEVOX エンジンは導入済みです（使うときに起動します）".into() } else { String::new() };
        });
        engine
    }

    fn marker(&self) -> PathBuf {
        self.dir.join("yuilivekit.ready.json")
    }

    pub fn installed(&self) -> bool {
        self.dir.join("run.exe").is_file() && self.marker().is_file()
    }

    pub fn state(&self) -> VoicevoxState {
        self.state.lock().unwrap().clone()
    }

    fn update(&self, change: impl FnOnce(&mut VoicevoxState)) {
        let snapshot = {
            let mut state = self.state.lock().unwrap();
            change(&mut state);
            state.clone()
        };
        (self.publish)(snapshot);
    }

    async fn answers(&self) -> bool {
        self.http.get(format!("{}/version", self.base)).send().await.is_ok_and(|r| r.status().is_success())
    }

    /// Checks whether VOICEVOX answers, and shows it.
    pub async fn refresh(&self) -> VoicevoxState {
        let running = self.answers().await;
        self.update(|s| s.running = running);
        self.state()
    }

    /// Makes sure VOICEVOX answers, starting the installed engine if nothing does.
    pub async fn ensure_running(&self) -> Result<(), String> {
        if self.answers().await {
            self.update(|s| s.running = true);
            return Ok(());
        }
        let _guard = self.starting.lock().await;
        if self.answers().await {
            return Ok(());
        }
        if !self.installed() {
            self.update(|s| s.running = false);
            return Err("VOICEVOXに接続できません。「読み上げ」タブでVOICEVOXエンジンを導入するか、VOICEVOXを起動してください".into());
        }
        self.spawn()?;
        self.update(|s| s.status = "VOICEVOX エンジンを起動中…".into());
        let deadline = tokio::time::Instant::now() + START_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if self.answers().await {
                self.update(|s| {
                    s.running = true;
                    s.status = "VOICEVOX エンジンを起動しました".into();
                });
                return Ok(());
            }
            if self.child.lock().unwrap().is_none() {
                break;
            }
        }
        self.close();
        self.update(|s| {
            s.running = false;
            s.status = "VOICEVOX エンジンを起動できませんでした".into();
        });
        Err("VOICEVOX エンジンを起動できませんでした。ほかのアプリがポート 50021 を使っていないか確認してください".into())
    }

    fn spawn(&self) -> Result<(), String> {
        let port = self.base.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()).unwrap_or(50021);
        let mut command = std::process::Command::new(self.dir.join("run.exe"));
        command
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .current_dir(&self.dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn().map_err(|e| format!("VOICEVOX エンジンを起動できません: {e}"))?;
        *self.child.lock().unwrap() = Some(child.id());
        // Forget the engine once it exits, so a later call can start it again.
        let engine_child = self.child.clone();
        std::thread::spawn(move || {
            let pid = child.id();
            let _ = child.wait();
            let mut current = engine_child.lock().unwrap();
            if *current == Some(pid) {
                *current = None;
            }
        });
        Ok(())
    }

    /// Ends the engine this app started (not a VOICEVOX the user runs).
    pub fn close(&self) {
        let pid = self.child.lock().unwrap().take();
        if let Some(pid) = pid {
            crate::worker::kill_tree(pid);
            self.update(|s| {
                s.running = false;
                s.status = "VOICEVOX エンジンを止めました".into();
            });
        }
    }

    pub async fn install(&self) -> Result<(), String> {
        {
            let mut state = self.state.lock().unwrap();
            if state.busy {
                return Err("導入中です。終わるまでお待ちください".into());
            }
            state.busy = true;
        }
        self.update(|s| s.status = "VOICEVOX エンジンの導入を準備中".into());
        let result = self.install_inner().await;
        let installed = self.installed();
        self.update(|s| {
            s.busy = false;
            s.installed = installed;
            s.status = match &result {
                Ok(()) => "VOICEVOX エンジンを導入しました".into(),
                Err(e) => format!("導入できませんでした。再試行できます: {e}"),
            };
        });
        result
    }

    async fn install_inner(&self) -> Result<(), String> {
        self.close();
        let parent = self.dir.parent().ok_or("導入先がありません")?.to_path_buf();
        std::fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
        let archive = parent.join("voicevox_engine.vvpp");
        download_checked(ENGINE.url, ENGINE.sha256, &archive, |size| {
            self.update(|s| s.status = format!("VOICEVOX エンジンを取得中: {:.0} MB / 約 1,895 MB", size as f64 / 1_048_576.0));
        })
        .await?;

        self.update(|s| s.status = "VOICEVOX エンジンを展開中（数分かかります）".into());
        let unpacked = parent.join("voicevox.unpacking");
        let _ = std::fs::remove_dir_all(&unpacked);
        std::fs::create_dir_all(&unpacked).map_err(|e| e.to_string())?;
        // Windows' own tar reads zip files and is much faster than Expand-Archive on 2 GB.
        let tar = PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into())).join("System32\\tar.exe");
        let mut command = tokio::process::Command::new(tar);
        command.arg("-xf").arg(&archive).arg("-C").arg(&unpacked).kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        let status = command.status().await.map_err(|e| format!("展開できません: {e}"))?;
        if !status.success() {
            let _ = std::fs::remove_dir_all(&unpacked);
            return Err(format!("展開に失敗しました ({})", status.code().unwrap_or(-1)));
        }
        let engine_root = find_run_exe(&unpacked, 2).ok_or("展開したファイルに run.exe がありません")?;

        let _ = std::fs::remove_dir_all(&self.dir);
        std::fs::rename(&engine_root, &self.dir).map_err(|e| format!("導入先に移せません: {e}"))?;
        std::fs::write(self.marker(), format!(r#"{{"version":"{VERSION}"}}"#)).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_dir_all(&unpacked);
        let _ = std::fs::remove_file(&archive);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yuilivekit-voicevox-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn without_an_engine_or_a_running_voicevox_it_says_what_to_do() {
        let dir = temp("missing");
        let engine = Engine::new(dir.join("engine"), "http://127.0.0.1:9", |_| {});
        assert!(!engine.installed());
        let error = engine.ensure_running().await.unwrap_err();
        assert!(error.contains("導入"), "{error}");
        assert!(!engine.refresh().await.running);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_voicevox_that_already_answers_is_used_as_it_is() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = axum::Router::new().route("/version", axum::routing::get(|| async { "\"0.26.1\"" }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = temp("external");
        let engine = Engine::new(dir.join("engine"), &format!("http://127.0.0.1:{port}"), |_| {});
        engine.ensure_running().await.unwrap();
        assert!(engine.state().running);
        assert!(engine.child.lock().unwrap().is_none(), "nothing was started");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_engine_folder_is_found_inside_the_archive() {
        let dir = temp("find");
        std::fs::create_dir_all(dir.join("windows-cpu")).unwrap();
        std::fs::write(dir.join("windows-cpu/run.exe"), "").unwrap();
        assert_eq!(find_run_exe(&dir, 2), Some(dir.join("windows-cpu")));
        assert_eq!(find_run_exe(&dir.join("nothing"), 2), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
