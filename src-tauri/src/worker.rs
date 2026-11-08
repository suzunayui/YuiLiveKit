//! A Python worker that speaks JSON Lines on stdin and stdout. Stderr is drained and never
//! logged, since it can hold recognized speech or chat text.

use std::{collections::HashMap, path::Path, process::Stdio};

use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::mpsc,
};

pub struct Worker {
    child: Child,
    stdin: ChildStdin,
}

impl Worker {
    /// Starts the worker. Each JSON line it prints arrives on the returned receiver; the
    /// receiver closes when the process ends.
    pub fn spawn(program: &Path, args: &[String], env: &HashMap<String, String>, cwd: &Path) -> Result<(Self, mpsc::UnboundedReceiver<Value>), String> {
        let mut command = Command::new(program);
        command
            .args(args)
            .envs(env)
            .env("PYTHONUTF8", "1")
            .env("PYTHONNOUSERSITE", "1")
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        // A damaged executable makes spawn fail here; report it instead of crashing (K11).
        let mut child = command.spawn().map_err(|e| format!("処理を起動できません: {e}"))?;
        let stdin = child.stdin.take().ok_or("stdin")?;
        let stdout = child.stdout.take().ok_or("stdout")?;
        let stderr = child.stderr.take().ok_or("stderr")?;
        let (sender, receiver) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if sender.send(value).is_err() {
                        break;
                    }
                }
            }
        });
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(_)) = lines.next_line().await {}
        });
        Ok((Self { child, stdin }, receiver))
    }

    pub async fn send(&mut self, value: &Value) -> Result<(), String> {
        let mut line = value.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await.map_err(|e| e.to_string())?;
        self.stdin.flush().await.map_err(|e| e.to_string())
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Ends the worker and anything it started (the translation worker runs llama.cpp).
    pub fn kill(mut self) {
        if let Some(pid) = self.child.id() {
            kill_tree(pid);
        }
        let _ = self.child.start_kill();
    }
}

pub fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(0x0800_0000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    let _ = pid;
}
