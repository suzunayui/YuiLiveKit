//! Installs what subtitles and translation need, only when asked: an app-private Python,
//! pinned libraries and llama.cpp. Every download is checked against a fixed SHA-256.
//! Ported from the Electron version's runtime/manager.js.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

struct Asset {
    url: &'static str,
    sha256: &'static str,
}

const PYTHON: Asset = Asset {
    url: "https://www.python.org/ftp/python/3.13.15/python-3.13.15-embed-amd64.zip",
    sha256: "d1f04d990aee1253d8569e8e5104e30fa9f5fa830899f14843448872d936a2cf",
};
const PIP: Asset = Asset {
    url: "https://files.pythonhosted.org/packages/f3/6e/1736e5b4ae2b778ef2f81c47d797de9f891d4d8acb047a24ca37a60294dd/pip-26.2.1-py3-none-any.whl",
    sha256: "71138adf1f4ca900cdb7d289c21b7494329f2332b6d85f0e1c42108c0384ed3e",
};
const LLAMA: Asset = Asset {
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b11429/llama-b11429-bin-win-vulkan-x64.zip",
    sha256: "1bfe78ad9168b79fa02bf67f6af9f5e17a966d824d77238517f7bef12ac73b36",
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    Speech,
    Cuda,
    Translation,
}

impl Feature {
    fn name(self) -> &'static str {
        match self {
            Feature::Speech => "speech",
            Feature::Cuda => "cuda",
            Feature::Translation => "translation",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RuntimeState {
    pub busy: bool,
    pub installing: Option<Feature>,
    pub status: String,
    pub speech: bool,
    pub cuda: bool,
    pub translation: bool,
}

/// Where things live. `source` holds the Python files shipped with the app.
#[derive(Clone)]
pub struct Paths {
    pub root: PathBuf,
    pub source: PathBuf,
}

impl Paths {
    pub fn python(&self) -> PathBuf {
        self.root.join("python").join("python.exe")
    }

    fn python_dir(&self) -> PathBuf {
        self.root.join("python")
    }

    pub fn llama(&self) -> PathBuf {
        self.root.join("llama").join("llama-server.exe")
    }

    fn backends(&self) -> PathBuf {
        self.root.join("backends")
    }

    pub fn worker(&self, feature: Feature) -> PathBuf {
        let dir = if feature == Feature::Translation { "translation" } else { "subtitles" };
        self.backends().join(dir).join("worker.py")
    }

    fn marker(&self, feature: Feature) -> PathBuf {
        self.root.join(format!("{}.ready.json", feature.name()))
    }
}

pub fn ready(paths: &Paths, feature: Feature) -> bool {
    let marker_ok = std::fs::read_to_string(paths.marker(feature))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .is_some_and(|v| v["version"] == 1);
    if !paths.python().exists() || !marker_ok {
        return false;
    }
    match feature {
        Feature::Speech => paths.worker(Feature::Speech).exists(),
        Feature::Translation => paths.llama().exists() && paths.worker(Feature::Translation).exists(),
        Feature::Cuda => {
            let nvidia = paths.python_dir().join("Lib/site-packages/nvidia");
            nvidia.join("cublas/bin/cublas64_12.dll").exists() && nvidia.join("cudnn/bin/cudnn64_9.dll").exists()
        }
    }
}

/// The search path holds absolute paths, so it is written again on every start, in case the
/// data folder moved (K12).
pub fn write_search_path(paths: &Paths) -> std::io::Result<()> {
    let lines = [
        "python313.zip".to_string(),
        ".".to_string(),
        "Lib/site-packages".to_string(),
        paths.backends().join("subtitles").display().to_string(),
        paths.backends().join("translation").display().to_string(),
        "import site".to_string(),
        String::new(),
    ];
    std::fs::write(paths.python_dir().join("python313._pth"), lines.join("\n"))
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == "__pycache__" || name_str.ends_with(".pyc") || name_str.starts_with("requirements") {
            continue;
        }
        let target = to.join(&name);
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Runs at app start: fixes the search path and refreshes the worker scripts of installed features.
pub fn prepare(paths: &Paths) {
    if paths.python_dir().join("bootstrap.ready").exists() {
        let _ = write_search_path(paths);
    }
    for (feature, dir) in [(Feature::Speech, "subtitles"), (Feature::Translation, "translation")] {
        if ready(paths, feature) {
            let _ = copy_tree(&paths.source.join(dir), &paths.backends().join(dir));
        }
    }
}

pub struct Runtime {
    pub paths: Paths,
    state: Mutex<RuntimeState>,
    child: Mutex<Option<u32>>,
    publish: Box<dyn Fn(RuntimeState) + Send + Sync>,
}

impl Runtime {
    pub fn new(paths: Paths, publish: impl Fn(RuntimeState) + Send + Sync + 'static) -> Arc<Self> {
        prepare(&paths);
        let runtime = Arc::new(Self {
            paths,
            state: Mutex::new(RuntimeState { status: "使いたい機能の「導入」を押してください".into(), ..RuntimeState::default() }),
            child: Mutex::new(None),
            publish: Box::new(publish),
        });
        runtime.refresh();
        runtime
    }

    pub fn ready(&self, feature: Feature) -> bool {
        ready(&self.paths, feature)
    }

    pub fn state(&self) -> RuntimeState {
        self.state.lock().unwrap().clone()
    }

    fn refresh(&self) {
        let snapshot = {
            let mut state = self.state.lock().unwrap();
            state.speech = ready(&self.paths, Feature::Speech);
            state.cuda = ready(&self.paths, Feature::Cuda);
            state.translation = ready(&self.paths, Feature::Translation);
            state.clone()
        };
        (self.publish)(snapshot);
    }

    fn status(&self, text: impl Into<String>) {
        self.state.lock().unwrap().status = text.into();
        self.refresh();
    }

    async fn download(&self, asset: &Asset, name: &str) -> Result<PathBuf, String> {
        use futures_util::StreamExt;
        let target = self.paths.root.join(name);
        let response = reqwest::get(asset.url).await.map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            return Err(format!("ダウンロードに失敗しました ({})", response.status()));
        }
        let mut file = tokio::fs::File::create(&target).await.map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let mut size = 0usize;
        let mut last = std::time::Instant::now();
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            hash.update(&chunk);
            size += chunk.len();
            file.write_all(&chunk).await.map_err(|e| e.to_string())?;
            if last.elapsed().as_millis() > 700 {
                last = std::time::Instant::now();
                self.status(format!("{name} を取得中: {:.1} MB", size as f64 / 1_048_576.0));
            }
        }
        file.flush().await.map_err(|e| e.to_string())?;
        let digest: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if digest != asset.sha256 {
            let _ = tokio::fs::remove_file(&target).await;
            return Err("ダウンロードの検証に失敗しました。もう一度導入してください".into());
        }
        Ok(target)
    }

    /// Runs a command, showing its output lines as the status when `report` is set.
    async fn run(&self, program: &Path, args: &[String], report: bool) -> Result<(), String> {
        use tokio::io::AsyncBufReadExt;
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .current_dir(&self.paths.root)
            .env("PYTHONUTF8", "1")
            .env("PIP_DISABLE_PIP_VERSION_CHECK", "1")
            .env("PYTHONNOUSERSITE", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        *self.child.lock().unwrap() = child.id();
        let mut out = tokio::io::BufReader::new(child.stdout.take().ok_or("stdout")?).lines();
        let mut err = tokio::io::BufReader::new(child.stderr.take().ok_or("stderr")?).lines();
        let mut tail: Vec<String> = Vec::new();
        loop {
            let line = tokio::select! {
                line = out.next_line() => line,
                line = err.next_line() => line,
            };
            match line {
                Ok(Some(line)) if !line.trim().is_empty() => {
                    if report {
                        self.status(line.trim().chars().take(240).collect::<String>());
                    }
                    tail.push(line);
                    if tail.len() > 10 {
                        tail.remove(0);
                    }
                }
                Ok(Some(_)) => {}
                _ => break,
            }
        }
        let status = child.wait().await.map_err(|e| e.to_string())?;
        *self.child.lock().unwrap() = None;
        if status.success() {
            Ok(())
        } else {
            Err(format!("導入処理に失敗しました ({}): {}", status.code().unwrap_or(-1), tail.join(" / ").chars().take(700).collect::<String>()))
        }
    }

    async fn expand(&self, zip: &Path, target: &Path) -> Result<(), String> {
        let quote = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "''"));
        let powershell = PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()))
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let script = format!("Expand-Archive -LiteralPath {} -DestinationPath {} -Force", quote(zip), quote(target));
        self.run(&powershell, &["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), script], false).await
    }

    async fn bootstrap(&self) -> Result<(), String> {
        let dir = self.paths.python_dir();
        if dir.join("bootstrap.ready").exists() {
            return Ok(());
        }
        self.status("アプリ専用の実行環境を導入中");
        let zip = self.download(&PYTHON, "python.zip").await?;
        self.expand(&zip, &dir).await?;
        let pip = self.download(&PIP, "pip.zip").await?;
        self.expand(&pip, &dir).await?;
        std::fs::create_dir_all(dir.join("Lib/site-packages")).map_err(|e| e.to_string())?;
        write_search_path(&self.paths).map_err(|e| e.to_string())?;
        self.run(&self.paths.python(), &["-m".into(), "pip".into(), "--version".into()], false).await?;
        std::fs::write(dir.join("bootstrap.ready"), "3.13.15 / pip 26.2.1\n").map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(zip);
        let _ = std::fs::remove_file(pip);
        Ok(())
    }

    async fn install_inner(&self, feature: Feature) -> Result<(), String> {
        std::fs::create_dir_all(&self.paths.root).map_err(|e| e.to_string())?;
        self.bootstrap().await?;
        let backend = if feature == Feature::Translation { "translation" } else { "subtitles" };
        let requirements = if feature == Feature::Cuda { "requirements-cuda.txt" } else { "requirements.txt" };
        let list = self.paths.root.join("requirements").join(format!("{}.txt", feature.name()));
        std::fs::create_dir_all(list.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::copy(self.paths.source.join(backend).join(requirements), &list).map_err(|e| e.to_string())?;
        let vendor = self.paths.root.join("vendor");
        copy_tree(&self.paths.source.join("vendor"), &vendor).map_err(|e| e.to_string())?;
        self.status("必要なライブラリを導入中");
        let site = self.paths.python_dir().join("Lib/site-packages");
        let args: Vec<String> = [
            "-m", "pip", "install", "--upgrade", "--only-binary=:all:", "--no-warn-script-location", "--index-url", "https://pypi.org/simple",
        ]
        .iter()
        .map(|s| s.to_string())
        .chain(["--find-links".into(), vendor.display().to_string(), "--target".into(), site.display().to_string()])
        .chain(["-r".into(), list.display().to_string()])
        .collect();
        self.run(&self.paths.python(), &args, true).await?;
        if feature == Feature::Translation {
            let zip = self.download(&LLAMA, "llama.zip").await?;
            let dir = self.paths.llama().parent().unwrap().to_path_buf();
            self.expand(&zip, &dir).await?;
            let _ = std::fs::remove_file(zip);
            let _ = std::fs::copy(self.paths.source.join("licenses/LICENSE-llama.cpp.txt"), dir.join("LICENSE-llama.cpp.txt"));
        }
        if feature != Feature::Cuda {
            copy_tree(&self.paths.source.join(backend), &self.paths.backends().join(backend)).map_err(|e| e.to_string())?;
        }
        let check = match feature {
            Feature::Speech => "import sounddevice, faster_whisper, onnxruntime",
            Feature::Translation => "import langid, lingua, huggingface_hub",
            Feature::Cuda => "from pathlib import Path;import sys;p=Path(sys.prefix)/'Lib/site-packages/nvidia';assert (p/'cublas/bin/cublas64_12.dll').is_file() and (p/'cudnn/bin/cudnn64_9.dll').is_file()",
        };
        self.run(&self.paths.python(), &["-c".into(), check.into()], false).await?;
        std::fs::write(self.paths.marker(feature), r#"{"version":1}"#).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn install(&self, feature: Feature) -> Result<(), String> {
        {
            let mut state = self.state.lock().unwrap();
            if state.busy {
                return Err("ほかの導入が終わるまでお待ちください".into());
            }
            state.busy = true;
            state.installing = Some(feature);
            state.status = "導入を準備中".into();
        }
        self.refresh();
        let result = self.install_inner(feature).await;
        {
            let mut state = self.state.lock().unwrap();
            state.busy = false;
            state.installing = None;
            state.status = match &result {
                Ok(()) => "導入が完了しました".into(),
                Err(e) => format!("導入できませんでした。再試行できます: {e}"),
            };
        }
        self.refresh();
        result
    }

    /// Ends a running installer step when the app closes.
    pub fn close(&self) {
        if let Some(pid) = self.child.lock().unwrap().take() {
            crate::worker::kill_tree(pid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yuilivekit-runtime-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_search_path_follows_the_folder_and_workers_refresh_k12() {
        let root = temp("pth");
        let source = temp("pth-source");
        std::fs::create_dir_all(source.join("subtitles")).unwrap();
        std::fs::write(source.join("subtitles/worker.py"), "new").unwrap();
        std::fs::write(source.join("subtitles/requirements.txt"), "x").unwrap();
        let paths = Paths { root: root.clone(), source: source.clone() };
        std::fs::create_dir_all(root.join("python")).unwrap();
        std::fs::write(root.join("python/python.exe"), "").unwrap();
        std::fs::write(root.join("python/bootstrap.ready"), "").unwrap();
        std::fs::write(root.join("python/python313._pth"), "C:\\old\\place\n").unwrap();
        std::fs::create_dir_all(root.join("backends/subtitles")).unwrap();
        std::fs::write(root.join("backends/subtitles/worker.py"), "old").unwrap();
        std::fs::write(root.join("speech.ready.json"), r#"{"version":1}"#).unwrap();

        prepare(&paths);

        let pth = std::fs::read_to_string(root.join("python/python313._pth")).unwrap();
        assert!(pth.contains(&root.join("backends").join("subtitles").display().to_string()));
        assert!(!pth.contains("old"));
        assert_eq!(std::fs::read_to_string(root.join("backends/subtitles/worker.py")).unwrap(), "new");
        assert!(!root.join("backends/subtitles/requirements.txt").exists());
        assert!(ready(&paths, Feature::Speech));
        assert!(!ready(&paths, Feature::Translation), "llama.cpp is missing");
        assert!(!ready(&paths, Feature::Cuda));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(source);
    }

    #[test]
    fn nothing_is_ready_before_installing() {
        let root = temp("empty");
        let paths = Paths { root: root.clone(), source: root.clone() };
        prepare(&paths);
        assert!(!root.join("python/python313._pth").exists());
        for feature in [Feature::Speech, Feature::Cuda, Feature::Translation] {
            assert!(!ready(&paths, feature));
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
