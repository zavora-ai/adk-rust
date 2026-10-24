//! `[kernel] drivers` under `adk-rust up` (spec 3.2 §7.7, §8.2, K-I13): the drivers the user
//! config lists mount when the kernel starts, each once the person approves it at the
//! terminal. A driver already mounted as approved is not asked about again, and without a
//! terminal a changed one keeps its approved manifest until the person approves the change.
//!
//! The binary runs as a process with a scripted model; the drivers are lazy, so none of
//! their commands runs.

#![cfg(all(unix, feature = "scripted-model", feature = "persist"))]

mod common;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::Terminal;

const REPLY: &str = r#"[{"text": "ok"}]"#;
const EMAIL: &str = "id = \"email\"\ncommand = \"mcp-email\"\nargs = [\"--imap\"]\n\
                     [tools]\nlist_inbox = \"read\"\nsend_draft = \"send_external\"\n";

/// Writes the user's `harness.toml`, in the config directory of either platform, listing
/// `~/email.toml` and an inline calendar manifest.
fn user_config(home: &Path, calendar_partitions: &str) {
    let config = format!(
        "[kernel]\ndrivers = [\n  \"~/email.toml\",\n  {{ id = \"calendar\", command = \
         \"mcp-calendar\", partitions = {calendar_partitions}, tools = {{ list_events = \"read\" \
         }} }},\n]\n"
    );
    std::fs::write(home.join("email.toml"), EMAIL).unwrap();
    for dir in [".config/adk-rust", "Library/Application Support/adk-rust"] {
        let dir = home.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("harness.toml"), &config).unwrap();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn up_args<'a>(kernel: &'a str, bind: &'a str, ws: &'a str) -> Vec<&'a str> {
    vec!["up", "--home", kernel, "--bind", bind, "--dir", ws, "--model-script", REPLY]
}

/// Waits until something listens on `port`, or `exited` reports the process gone.
fn listening(port: u16, mut exited: impl FnMut() -> bool) {
    let started = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(!exited(), "up exited before it listened");
        assert!(started.elapsed() < common::TIMEOUT, "up did not listen on {port}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `adk-rust drivers list` against the kernel at `kernel`.
fn drivers_list(home: &Path, ws: &Path, kernel: &str) -> String {
    let output = common::run(&["drivers", "--home", kernel, "list"], home, ws);
    let text = common::text(&output);
    assert!(output.status.success(), "{text}");
    text
}

/// Stops the kernel on the terminal with Ctrl-C and waits for it to drain.
fn stop(mut terminal: Terminal) -> String {
    terminal.send("\x03");
    let (code, text) = terminal.wait();
    assert_eq!(code, Some(0), "{text}");
    text
}

#[test]
fn up_mounts_the_drivers_the_user_config_lists_once_each_is_approved() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("README.md"), "a project\n").unwrap();
    let kernel = home.path().join("kernel").display().to_string();
    let dir = ws.path().display().to_string();
    user_config(home.path(), "[\"default\"]");

    // First start: both drivers are shown for approval; the person mounts email only.
    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    let mut terminal = Terminal::spawn_in(&up_args(&kernel, &bind, &dir), home.path(), ws.path());
    terminal.expect("driver email");
    terminal.expect("command     mcp-email --imap");
    terminal.expect("send_draft");
    terminal.expect("Mount this driver? [y/N]");
    terminal.send("y\r");
    terminal.expect("driver calendar");
    terminal.expect("Mount this driver? [y/N]");
    terminal.send("n\r");
    terminal.expect("Not mounted.");
    terminal.expect("ADK-Rust kernel up on");
    listening(port, || false);
    let listed = drivers_list(home.path(), ws.path(), &kernel);
    assert!(listed.contains("email") && !listed.contains("calendar"), "{listed}");
    stop(terminal);

    // Second start: email is mounted as approved and not asked about; calendar is.
    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    let mut terminal = Terminal::spawn_in(&up_args(&kernel, &bind, &dir), home.path(), ws.path());
    terminal.expect("driver calendar");
    terminal.expect("Mount this driver? [y/N]");
    terminal.send("y\r");
    terminal.expect("ADK-Rust kernel up on");
    listening(port, || false);
    let listed = drivers_list(home.path(), ws.path(), &kernel);
    assert!(listed.contains("email") && listed.contains("calendar"), "{listed}");
    assert!(listed.contains("[default]") && !listed.contains("work"), "{listed}");
    let text = stop(terminal);
    assert!(!text.contains("driver email"), "email was approved before:\n{text}");

    // Third start, without a terminal, after calendar's manifest changed: nothing is asked
    // or mounted; the approved calendar keeps running as approved, and the change is noted.
    user_config(home.path(), "[\"default\", \"work\"]");
    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    let mut command = Command::new(env!("CARGO_BIN_EXE_adk-rust"));
    command
        .args(up_args(&kernel, &bind, &dir))
        .current_dir(ws.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    common::isolate(&mut command, home.path());
    let mut child = command.spawn().expect("adk-rust starts");
    listening(port, || child.try_wait().unwrap().is_some());
    let listed = drivers_list(home.path(), ws.path(), &kernel);
    assert!(listed.contains("[default]") && !listed.contains("work"), "{listed}");
    // SAFETY: the child is alive; SIGTERM asks `up` to drain and stop.
    unsafe { libc::kill(i32::try_from(child.id()).unwrap(), libc::SIGTERM) };
    let output = child.wait_with_output().unwrap();
    let text = common::text(&output);
    assert!(
        text.contains("note: driver `calendar` in [kernel] drivers changed since you approved it"),
        "{text}"
    );
    assert!(!text.contains("Mount this driver?") && !text.contains("`email`"), "{text}");
}
