//! Shared helpers for the computer-use live examples.
//!
//! These are deduplicated across the cross-platform `live_clipboard` example
//! and the macOS native-UI showcases (`live_form`, `live_background_finder`).
//! Each example includes this file with `#[path = "..."] mod support;`. Because
//! it lives in a subdirectory of `examples/` (and has no `main.rs`), Cargo does
//! not build it as its own example target.

#![allow(dead_code)]

use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::{Child, Command};

/// Unwrap the `{ response: { output } }` / `{ output }` envelope, if present.
pub fn output(value: &Value) -> &Value {
    value
        .get("response")
        .and_then(|value| value.get("output"))
        .or_else(|| value.get("output"))
        .unwrap_or(value)
}

/// Convert a JSON value into an object map, dropping null-valued entries.
pub fn object(value: Value) -> Result<Map<String, Value>, Box<dyn std::error::Error>> {
    let mut value = value.as_object().cloned().ok_or("expected an object")?;
    value.retain(|_, entry| !entry.is_null());
    Ok(value)
}

/// Read the system clipboard on macOS, Linux, or Windows and trim any trailing
/// newline added by the platform tool.
///
/// Used by the cross-platform live example to independently verify that the
/// governed graph actually wrote the expected text to the real clipboard.
pub async fn read_clipboard() -> Result<String, Box<dyn std::error::Error>> {
    let bytes = clipboard_bytes().await?;
    Ok(String::from_utf8(bytes)?.trim_end_matches(['\r', '\n']).to_string())
}

#[cfg(target_os = "macos")]
async fn clipboard_bytes() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    Ok(Command::new("pbpaste").output().await?.stdout)
}

#[cfg(target_os = "windows")]
async fn clipboard_bytes() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", "Get-Clipboard"])
        .output()
        .await?;
    Ok(output.stdout)
}

#[cfg(target_os = "linux")]
async fn clipboard_bytes() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    // Try Wayland first, then fall back to the common X11 clipboard tools.
    async fn try_read(bin: &str, args: &[&str]) -> Option<Vec<u8>> {
        let output = Command::new(bin).args(args).output().await.ok()?;
        output.status.success().then_some(output.stdout)
    }
    if let Some(bytes) = try_read("wl-paste", &["--no-newline"]).await {
        return Ok(bytes);
    }
    if let Some(bytes) = try_read("xclip", &["-selection", "clipboard", "-o"]).await {
        return Ok(bytes);
    }
    if let Some(bytes) = try_read("xsel", &["--clipboard", "--output"]).await {
        return Ok(bytes);
    }
    Err("install wl-clipboard, xclip, or xsel to verify the clipboard on Linux".into())
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
async fn clipboard_bytes() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    Err("clipboard verification is not supported on this platform".into())
}

/// Resolve the `computer-use-supervisor` package directory.
///
/// Prefers `COMPUTER_USE_SUPERVISOR_DIR`; otherwise derives it from the MCP
/// server entrypoint (`.../computer-use-mcp/dist/server.js`).
pub fn supervisor_dir(entrypoint: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(value) = std::env::var("COMPUTER_USE_SUPERVISOR_DIR") {
        return Ok(PathBuf::from(value));
    }
    let root = Path::new(entrypoint)
        .parent()
        .and_then(Path::parent)
        .ok_or("cannot derive computer-use-mcp root from entrypoint")?;
    Ok(root.join("packages/computer-use-supervisor"))
}

/// Launch the picture-in-picture supervisor approval window.
///
/// Prefers an explicit `COMPUTER_USE_ELECTRON` binary; otherwise runs Electron
/// via `npx`.
pub fn spawn_pip(
    directory: &Path,
    socket: &Path,
    token: &str,
    principal: &str,
    session_id: &str,
) -> Result<Child, Box<dyn std::error::Error>> {
    let mut command = if let Ok(electron) = std::env::var("COMPUTER_USE_ELECTRON") {
        let mut command = Command::new(electron);
        command.arg(".");
        command
    } else {
        let mut command = Command::new("npx");
        command.args(["--yes", "--package=electron@43.1.0", "electron", "."]);
        command
    };
    Ok(command
        .current_dir(directory)
        .env("COMPUTER_USE_SUPERVISOR_SOCKET", socket)
        .env("COMPUTER_USE_SUPERVISOR_TOKEN", token)
        .env("COMPUTER_USE_PRINCIPAL_ID", principal)
        .env("COMPUTER_USE_SESSION_ID", session_id)
        .env("COMPUTER_USE_SUPERVISOR_DEBUG", "true")
        .stdout(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?)
}
