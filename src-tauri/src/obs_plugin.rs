//! Installs the OBS plugin that adds the overlay as one browser source.
//! Same place and steps as YuiVoiceChanger: %ProgramData%\obs-studio\plugins, with an
//! administrator prompt only when the folder is not writable.

use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Command,
};

use serde::Serialize;

const NAME: &str = "yui-live-kit";

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginState {
    Installed,
    Outdated,
    Missing,
}

#[derive(Clone, Debug, Serialize)]
pub struct PluginStatus {
    pub state: PluginState,
    pub path: String,
    pub obs_running: bool,
}

/// `<ProgramData>\obs-studio\plugins\yui-live-kit\bin\64bit\yui-live-kit.dll`, or a test folder.
pub fn target_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("YUI_LIVE_KIT_OBS_PLUGIN_DIR") {
        return Some(PathBuf::from(dir).join(format!("{NAME}.dll")));
    }
    let root = std::env::var_os("ProgramData")?;
    Some(Path::new(&root).join("obs-studio").join("plugins").join(NAME).join("bin").join("64bit").join(format!("{NAME}.dll")))
}

pub fn obs_running() -> bool {
    let mut command = Command::new("tasklist");
    command.args(["/FI", "IMAGENAME eq obs64.exe", "/FO", "CSV", "/NH"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).to_ascii_lowercase().contains("\"obs64.exe\""))
        .unwrap_or(false)
}

pub fn status(bundled: &[u8]) -> PluginStatus {
    let path = target_path();
    let state = match path.as_deref().map(std::fs::read) {
        Some(Ok(installed)) if installed == bundled => PluginState::Installed,
        Some(Ok(_)) => PluginState::Outdated,
        _ => PluginState::Missing,
    };
    PluginStatus { state, path: path.map(|p| p.display().to_string()).unwrap_or_default(), obs_running: obs_running() }
}

fn write_atomically(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = target.parent().ok_or(ErrorKind::NotFound)?;
    std::fs::create_dir_all(dir)?;
    let temporary = dir.join(format!("{NAME}.{}.tmp", std::process::id()));
    std::fs::write(&temporary, bytes)?;
    let result = std::fs::rename(&temporary, target);
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02X}")).collect()
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Copies from a file the user's account wrote, checking its hash again after elevation so a
/// swapped file is never installed with administrator rights.
fn install_elevated(source: &Path, target: &Path, bytes: &[u8]) -> Result<(), String> {
    let script = format!(
        "$ErrorActionPreference='Stop'\n\
         $source={source}\n$target={target}\n\
         if ((Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash -ne '{hash}') {{ throw 'changed' }}\n\
         $dir=Split-Path -LiteralPath $target\n\
         [void][System.IO.Directory]::CreateDirectory($dir)\n\
         if ((Get-Item -LiteralPath $dir).Attributes -band [System.IO.FileAttributes]::ReparsePoint) {{ throw 'redirected' }}\n\
         Copy-Item -LiteralPath $source -Destination $target -Force\n\
         if ((Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash -ne '{hash}') {{ throw 'copy' }}",
        source = quote(&source.display().to_string()),
        target = quote(&target.display().to_string()),
        hash = sha256_hex(bytes),
    );
    let encoded: String = {
        let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        base64_encode(&utf16)
    };
    let elevate = format!(
        "$p=Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -Verb RunAs -WindowStyle Hidden -Wait -PassThru \
         -ArgumentList '-NoProfile','-NonInteractive','-EncodedCommand','{encoded}'; exit $p.ExitCode"
    );
    let status = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &elevate])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("導入できませんでした。管理者の確認で「はい」を選んで、もう一度お試しください".into())
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Installs the plugin. OBS must be closed so the DLL is not in use and the new one loads next start.
pub fn install(bundled: &[u8], scratch_dir: &Path) -> Result<PluginStatus, String> {
    if obs_running() {
        return Err("OBSを終了してから導入してください".into());
    }
    let target = target_path().ok_or("OBSのプラグインフォルダが見つかりません")?;
    match write_atomically(&target, bundled) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            std::fs::create_dir_all(scratch_dir).map_err(|e| e.to_string())?;
            let source = scratch_dir.join(format!("{NAME}.dll"));
            std::fs::write(&source, bundled).map_err(|e| e.to_string())?;
            let result = install_elevated(&source, &target, bundled);
            let _ = std::fs::remove_file(&source);
            result?;
        }
        Err(e) => return Err(format!("導入できませんでした: {e}")),
    }
    let status = status(bundled);
    if status.state == PluginState::Installed {
        Ok(status)
    } else {
        Err("導入したプラグインを確認できませんでした".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_base64_like_the_standard() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn installs_into_a_writable_folder_and_reports_the_state() {
        // Only this test sets the variable, so it cannot race another test.
        let dir = std::env::temp_dir().join(format!("yuilivekit-plugin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::set_var("YUI_LIVE_KIT_OBS_PLUGIN_DIR", dir.join("bin")) };
        assert_eq!(status(b"v1").state, PluginState::Missing);
        if !obs_running() {
            assert_eq!(install(b"v1", &dir.join("scratch")).unwrap().state, PluginState::Installed);
            assert_eq!(status(b"v2").state, PluginState::Outdated);
            assert_eq!(install(b"v2", &dir.join("scratch")).unwrap().state, PluginState::Installed);
        }
        unsafe { std::env::remove_var("YUI_LIVE_KIT_OBS_PLUGIN_DIR") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_default_place_is_the_obs_plugin_folder() {
        if std::env::var("YUI_LIVE_KIT_OBS_PLUGIN_DIR").is_err() {
            let path = target_path().unwrap().display().to_string();
            assert!(path.ends_with(r"obs-studio\plugins\yui-live-kit\bin\64bit\yui-live-kit.dll"), "{path}");
        }
    }
}
