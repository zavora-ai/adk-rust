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

/// The config directories the binary may read under `home`, one per platform.
fn config_dirs(home: &Path) -> [std::path::PathBuf; 2] {
    [home.join(".config/adk-rust"), home.join("Library/Application Support/adk-rust")]
}

/// First-run answers already given: 22:00–07:00 and $5 a day.
fn answered(home: &Path) {
    std::fs::create_dir_all(home).unwrap();
    std::fs::write(
        home.join("personal.toml"),
        "quiet_time = \"22:00-07:00\"\ndaily_spending_limit_usd = 5.0\n",
    )
    .unwrap();
}

/// Review findings 4 and 16: a workspace `.adk/harness.toml` may lower the first-run daily
/// spending limit and never raise it.
#[test]
fn a_workspace_file_never_raises_the_first_run_limit() {
    let home = tempfile::tempdir().unwrap();
    let repo = repository();
    let kernel = home.path().join("k");
    answered(&kernel);
    let adk = kernel.join("workspace/.adk");
    for (cap, shown) in [("500.0", "$5.00 a day"), ("2.0", "$2.00 a day")] {
        std::fs::create_dir_all(&adk).unwrap();
        std::fs::write(
            adk.join("harness.toml"),
            format!("[kernel.quotas]\ndaily_spend_cap_usd = {cap}\n"),
        )
        .unwrap();
        let port = free_port();
        let args = up_args(&kernel, port);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut terminal = Terminal::spawn_in(&args, home.path(), repo.path());
        terminal.expect(&format!("quiet time 22:00–07:00 · daily spending limit {shown}"));
        wait_listening(port, &terminal);
        terminal.send("\x03");
        terminal.wait();
    }
}

/// Review finding 10: the current directory's `.adk/harness.toml` plays no part in the
/// personal preset, under `up` and `up --install`.
#[test]
fn the_current_directorys_kernel_config_does_not_stop_the_personal_preset() {
    let home = tempfile::tempdir().unwrap();
    let repo = repository();
    std::fs::create_dir_all(repo.path().join(".adk")).unwrap();
    std::fs::write(repo.path().join(".adk/harness.toml"), "[kernel]\nmax_concurrent_runs = 0\n")
        .unwrap();
    let kernel = home.path().join("k");
    answered(&kernel);
    let port = free_port();
    let args = up_args(&kernel, port);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut terminal = Terminal::spawn_in(&args, home.path(), repo.path());
    wait_listening(port, &terminal);
    terminal.send("\x03");
    terminal.wait();

    let output = run(
        &["up", "--install", "--print", "--preset", "personal", "--home", kernel.to_str().unwrap()],
        home.path(),
        repo.path(),
    );
    assert!(output.status.success(), "{}", text(&output));
}

/// Review findings 11, 13 and 15: the REPL shows no shell, in its banner and in `/sandbox`,
/// and shows the limits `adk-rust up` keeps — the user config's `[kernel]` table winning.
#[test]
#[cfg_attr(feature = "tui", ignore = "`tui` replaces the REPL with the terminal surface")]
fn the_repl_shows_no_shell_and_the_limits_in_force() {
    let home = tempfile::tempdir().unwrap();
    let repo = repository();
    for config in config_dirs(home.path()) {
        answered(&config);
        std::fs::write(config.join("harness.toml"), "[kernel]\nquiet_hours = \"23:00-06:00\"\n")
            .unwrap();
    }
    let mut terminal = Terminal::spawn_in(
        &["--preset", "personal", "--model-script", "[]"],
        home.path(),
        repo.path(),
    );
    terminal.expect("Shell     : none");
    terminal.expect("Quiet time: 23:00–06:00 (kept by adk-rust up)");
    terminal.expect("Spending  : daily spending limit $5.00 a day (kept by adk-rust up)");
    terminal.expect("You >");
    terminal.send("/sandbox\r");
    terminal.expect("shell: none");
    terminal.send("/sandbox off\r");
    terminal.expect("rules and approvals granted for the other shell do not apply");
    let shown = terminal.finish();
    assert!(!shown.contains("shell: host"), "{shown}");
}

/// Review finding 8: `serve --preset personal` shows both first-run limits.
#[test]
fn serve_with_the_personal_preset_shows_both_limits() {
    let home = tempfile::tempdir().unwrap();
    let repo = repository();
    for config in config_dirs(home.path()) {
        answered(&config);
    }
    let port = free_port();
    let mut terminal = Terminal::spawn_in(
        &["serve", "--preset", "personal", "--port", &port.to_string(), "--model-script", "[]"],
        home.path(),
        repo.path(),
    );
    terminal.expect("Quiet time: 22:00–07:00 (kept by adk-rust up)");
    terminal.expect("Spending: daily spending limit $5.00 a day (kept by adk-rust up)");
    wait_listening(port, &terminal);
    terminal.send("\x03");
    terminal.wait();
}

/// Review of #1045, finding 3: a `[kernel]` error stops only `adk-rust up`, which runs the
/// kernel; the REPL notes it and starts with the first-run answers.
#[test]
#[cfg_attr(feature = "tui", ignore = "`tui` replaces the REPL with the terminal surface")]
fn a_kernel_config_error_does_not_stop_the_repl() {
    let home = tempfile::tempdir().unwrap();
    let repo = repository();
    for config in config_dirs(home.path()) {
        answered(&config);
        std::fs::write(config.join("harness.toml"), "[kernel]\nmax_concurrent_runs = 0\n").unwrap();
    }
    let mut terminal = Terminal::spawn_in(
        &["--preset", "personal", "--model-script", "[]"],
        home.path(),
        repo.path(),
    );
    terminal.expect("Spending  : daily spending limit $5.00 a day (kept by adk-rust up)");
    terminal.expect("the limits shown are the first-run answers");
    terminal.expect("You >");
    terminal.finish();
}

/// Review of #1045, finding 4: the REPL reads the `personal.toml` `adk-rust up` keeps, in the
/// user config's `[kernel] home`, rather than asking again elsewhere.
#[test]
#[cfg_attr(feature = "tui", ignore = "`tui` replaces the REPL with the terminal surface")]
fn the_repl_reads_the_answers_up_keeps() {
    let home = tempfile::tempdir().unwrap();
    let repo = repository();
    let kernel = home.path().join("assistant");
    answered(&kernel);
    std::fs::write(
        kernel.join("personal.toml"),
        "quiet_time = \"21:00-06:00\"\ndaily_spending_limit_usd = 50.0\n",
    )
    .unwrap();
    for config in config_dirs(home.path()) {
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("harness.toml"),
            format!("[kernel]\nhome = {:?}\n", kernel.display().to_string()),
        )
        .unwrap();
    }
    let mut terminal = Terminal::spawn_in(
        &["--preset", "personal", "--model-script", "[]"],
        home.path(),
        repo.path(),
    );
    terminal.expect("Quiet time: 21:00–06:00 (kept by adk-rust up)");
    terminal.expect("Spending  : daily spending limit $50.00 a day (kept by adk-rust up)");
    terminal.expect("You >");
    let shown = terminal.finish();
    assert!(!shown.contains("Daily spending limit —"), "asked again: {shown}");
}
