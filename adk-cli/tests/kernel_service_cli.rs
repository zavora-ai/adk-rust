//! `adk-rust up --install --print` renders this platform's service definition without
//! installing anything, and `--install` refuses what a definition must not carry (spec 3.2
//! §7.12, §10 W3). Real installation runs in the `Kernel Service` workflow only.

#![cfg(feature = "harness")]

use std::path::Path;
use std::process::{Command, Output, Stdio};

/// Runs the binary in `dir` with a private home, no provider key and no terminal.
fn adk(args: &[&str], dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_adk-rust"))
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .env("RUST_LOG", "off")
        .env_remove("ADK_SERVE_TOKEN")
        .stdin(Stdio::null())
        .output()
        .expect("adk-rust runs")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn print_renders_the_platform_definition_and_installs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("kernel home");
    let home_arg = home.to_str().unwrap();
    let output =
        adk(&["up", "--install", "--print", "--home", home_arg, "--mode", "ask"], dir.path());
    assert!(output.status.success(), "{}", text(&output));
    let definition = String::from_utf8_lossy(&output.stdout);
    // The plist's label and the Windows service carry the service's name; a systemd unit's
    // name is its file name, so the unit's text does not.
    if cfg!(target_os = "macos") {
        assert!(definition.starts_with("<?xml"), "{definition}");
        for expected in [
            "<key>Label</key>\n    <string>adk-rust-kernel-".to_string(),
            format!("<string>{home_arg}</string>"),
            "<string>up</string>".to_string(),
            "<string>--mode</string>\n        <string>ask</string>".to_string(),
            "<key>SuccessfulExit</key>\n        <false/>".to_string(),
            format!("<string>{home_arg}/kernel.log</string>"),
        ] {
            assert!(definition.contains(&expected), "{expected} in\n{definition}");
        }
        assert!(!dir.path().join("Library/LaunchAgents").exists(), "nothing is installed");
    } else if cfg!(target_os = "linux") {
        for expected in [
            format!("--home \"{home_arg}\""),
            " up --mode ask ".to_string(),
            "Restart=on-failure\n".to_string(),
            format!("StandardOutput=append:{home_arg}/kernel.log\n"),
            "WantedBy=default.target\n".to_string(),
        ] {
            assert!(definition.contains(&expected), "{expected} in\n{definition}");
        }
        assert!(!dir.path().join(".config/systemd").exists(), "nothing is installed");
    } else if cfg!(windows) {
        for expected in [
            "service: adk-rust-kernel-".to_string(),
            " up --mode ask ".to_string(),
            "--windows-service adk-rust-kernel-".to_string(),
            "start: automatic (delayed), at boot\n".to_string(),
            format!("log: {home_arg}\\kernel.log\n"),
        ] {
            assert!(definition.contains(&expected), "{expected} in\n{definition}");
        }
    }
    assert!(!home.exists(), "printing creates no kernel home");
}

#[test]
fn install_refuses_secrets_and_a_bind_beyond_loopback() {
    let dir = tempfile::tempdir().unwrap();
    for (args, expected) in [
        (&["up", "--install", "--print", "--auth-token", "t0k3n"][..], "definition"),
        (&["--api-key", "k3y", "up", "--install", "--print"][..], "definition"),
        (&["up", "--install", "--print", "--bind", "0.0.0.0:8080"][..], "loopback"),
    ] {
        let output = adk(args, dir.path());
        let answer = text(&output);
        assert!(!output.status.success(), "{args:?}: {answer}");
        assert!(answer.contains(expected), "{args:?}: {answer}");
        assert!(!answer.contains("t0k3n") && !answer.contains("k3y"), "{args:?}: {answer}");
    }
    let output = adk(&["up", "--print"], dir.path());
    assert!(!output.status.success(), "--print needs --install: {}", text(&output));
}
