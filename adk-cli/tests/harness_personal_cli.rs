//! `--preset personal` end to end (spec 3.3 §10 H1, §13 Q7): `adk-rust up` works in a
//! folder under the kernel home rather than the current repository, asks for quiet time and a
//! daily spending limit at its first run, and shows both on every start.

#![cfg(all(unix, feature = "scripted-model", feature = "harness", feature = "persist"))]

mod common;

use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use common::{Terminal, run, text};

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A code repository the command is started in, which the preset must not use.
fn repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    dir
}

fn up_args(kernel: &Path, port: u16) -> Vec<String> {
    [
        "up",
        "--preset",
        "personal",
        "--home",
        kernel.to_str().unwrap(),
        "--bind",
        &format!("127.0.0.1:{port}"),
        "--model-script",
        "[]",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn wait_listening(port: u16, terminal: &Terminal) {
    let started = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(started.elapsed() < common::TIMEOUT, "up did not listen:\n{}", terminal.text());
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn up_with_the_personal_preset_asks_once_and_works_outside_the_repository() {
    let home = tempfile::tempdir().unwrap();
    let repo = repository();
    let kernel = home.path().join("k");
    let port = free_port();
    let args = up_args(&kernel, port);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let mut first = Terminal::spawn_in(&args, home.path(), repo.path());
    first.expect("Quiet time — ");
    first.send("22:30-06:30\r");
    first.expect("Daily spending limit — ");
    first.send("\r");
    first.expect("quiet time 22:30–06:30 · daily spending limit $5.00 a day");
    wait_listening(port, &first);
    // The kernel's banner names no shell (the port listens before it prints).
    first.expect("· shell none");
    first.send("\x03");
    let (_, shown) = first.wait();

    let workspace = kernel.join("workspace");
    assert!(workspace.is_dir(), "{shown}");
    assert!(
        shown.contains(&format!("work  {}", workspace.canonicalize().unwrap().display())),
        "{shown}"
    );
    assert!(
        !shown.contains(&repo.path().display().to_string()),
        "the repository is unused:\n{shown}"
    );
    let saved = std::fs::read_to_string(kernel.join("personal.toml")).unwrap();
    assert!(saved.contains("quiet_time = \"22:30-06:30\""), "{saved}");
    assert!(saved.contains("daily_spending_limit_usd = 5.0"), "{saved}");

    // The next start reads the answers and still shows both; the user config's `[kernel]`
    // table wins where it sets one.
    for config in [
        home.path().join(".config/adk-rust"),
        home.path().join("Library/Application Support/adk-rust"),
    ] {
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("harness.toml"), "[kernel]\nquiet_hours = \"23:00-06:00\"\n")
            .unwrap();
    }
    let port = free_port();
    let args = up_args(&kernel, port);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut second = Terminal::spawn_in(&args, home.path(), repo.path());
    second.expect("quiet time 23:00–06:00 · daily spending limit $5.00 a day");
    wait_listening(port, &second);
    second.send("\x03");
    let (_, shown) = second.wait();
    assert!(!shown.contains("Quiet time — "), "asked again:\n{shown}");
}

#[test]
fn a_first_run_without_answers_refuses_and_names_the_file() {
    let home = tempfile::tempdir().unwrap();
    let repo = repository();
    let output = run(
        &["code", "note this", "--preset", "personal", "--model-script", "[]"],
        home.path(),
        repo.path(),
    );
    let shown = text(&output);
    assert!(!output.status.success(), "{shown}");
    assert!(shown.contains("no answer came"), "{shown}");
    assert!(shown.contains("personal.toml"), "{shown}");
}

#[test]
fn a_personal_kernel_service_runs_in_the_personal_workspace() {
    let home = tempfile::tempdir().unwrap();
    let repo = repository();
    let kernel = home.path().join("k");
    let output = run(
        &["up", "--install", "--print", "--preset", "personal", "--home", kernel.to_str().unwrap()],
        home.path(),
        repo.path(),
    );
    let shown = text(&output);
    assert!(output.status.success(), "{shown}");
    assert!(shown.contains("personal"), "the preset is kept:\n{shown}");
    assert!(shown.contains(&kernel.join("workspace").display().to_string()), "{shown}");
    assert!(!shown.contains(&repo.path().display().to_string()), "{shown}");
}
