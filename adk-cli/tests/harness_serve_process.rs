//! `adk-rust serve` as a process (spec 3.1 §8.2, §13 Q5): it starts, listens, and names
//! where plans are kept.

#![cfg(all(unix, feature = "scripted-model", feature = "harness", feature = "persist"))]

mod common;

use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn serve_starts_and_names_where_plans_are_kept() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("README.md"), "a project\n").unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut command = Command::new(env!("CARGO_BIN_EXE_adk-rust"));
    command
        .args(["serve", "--port", &port.to_string(), "--dir"])
        .arg(ws.path())
        .args(["--model-script", "[]"])
        .current_dir(ws.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    common::isolate(&mut command, home.path());
    let mut child = command.spawn().expect("adk-rust starts");
    let started = Instant::now();
    let listening = loop {
        if child.try_wait().unwrap().is_some() {
            break false;
        }
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break true;
        }
        if started.elapsed() > common::TIMEOUT {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    let shown = common::text(&output);
    assert!(listening, "serve did not listen:\n{shown}");
    assert!(shown.contains("plans checkpointed in harness.db"), "{shown}");
}
