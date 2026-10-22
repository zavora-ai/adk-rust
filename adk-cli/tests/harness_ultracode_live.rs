//! `adk-rust ultracode` against live models through the built binary (spec 3.1 §7.10, §11):
//! `claude-sonnet-5-5` with `ANTHROPIC_API_KEY`, `gemini-3.5-flash-lite` with
//! `GOOGLE_API_KEY`. Keys come from the environment, or from the file `HARNESS_ENV_FILE`
//! names, else the repository's `.env`; a provider without a key is skipped, and
//! `ULTRACODE_LIVE_PROVIDER=anthropic|gemini` runs one provider only (run Gemini's free tier
//! with `--test-threads 1`).
//!
//! ```text
//! cargo nextest run -p adk-cli --features scripted-model --run-ignored only -E 'test(live_ultracode)'
//! ```

#![cfg(all(unix, feature = "harness", feature = "persist"))]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

/// `KEY=VALUE` lines of the env file, without printing any value.
fn env_file() -> BTreeMap<String, String> {
    let path = std::env::var("HARNESS_ENV_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".env"));
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return None;
            }
            let (key, value) = line.strip_prefix("export ").unwrap_or(line).split_once('=')?;
            let value = value.trim().trim_matches('"').trim_matches('\'');
            (!value.is_empty()).then(|| (key.trim().to_string(), value.to_string()))
        })
        .collect()
}

/// The providers with a key: `(provider, model, key variable, key)`.
fn providers() -> Vec<(&'static str, &'static str, &'static str, String)> {
    let file = env_file();
    let key = |name: &str| {
        file.get(name).cloned().or_else(|| std::env::var(name).ok()).filter(|key| !key.is_empty())
    };
    let mut out = Vec::new();
    if let Some(value) = key("ANTHROPIC_API_KEY") {
        out.push(("anthropic", "claude-sonnet-5-5", "ANTHROPIC_API_KEY", value));
    }
    if let Some(value) = key("GOOGLE_API_KEY") {
        out.push(("gemini", "gemini-3.5-flash-lite", "GOOGLE_API_KEY", value));
    }
    if let Ok(only) = std::env::var("ULTRACODE_LIVE_PROVIDER") {
        out.retain(|(provider, ..)| *provider == only);
    }
    if out.is_empty() {
        println!("skipped: no ANTHROPIC_API_KEY or GOOGLE_API_KEY");
    }
    out
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Runs `ultracode` on `provider` in `ws` and returns the output and the receipt.
fn run(
    (provider, model, key_name, key): &(&str, &str, &str, String),
    ws: &Path,
    home: &Path,
    task: &str,
) -> (std::process::Output, Value) {
    let receipt = home.join("receipt.json");
    // The implementer's `cargo test` finds the installed toolchain through the isolated home.
    if let Ok(real) = std::env::var("HOME") {
        for dir in [".rustup", ".cargo"] {
            let source = Path::new(&real).join(dir);
            if source.exists() {
                std::os::unix::fs::symlink(&source, home.join(dir)).ok();
            }
        }
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_adk-rust"));
    command
        .args(["--provider", provider, "--model", model, "ultracode", task])
        // The fixtures need no web search, so the run makes only ultracode's own model calls.
        .args(["--ephemeral", "--mode", "auto", "--yes", "--no-memory", "--search", "off", "--dir"])
        .arg(ws)
        .arg("--receipt")
        .arg(&receipt)
        .current_dir(ws)
        .stdin(Stdio::null());
    common::isolate(&mut command, home);
    for name in ["ANTHROPIC_API_KEY", "GOOGLE_API_KEY", "GEMINI_API_KEY", "OPENAI_API_KEY"] {
        command.env_remove(name);
    }
    command.env(key_name, key).env("CARGO_TARGET_DIR", home.join("target"));
    let output = command.output().expect("adk-rust runs");
    let text = std::fs::read_to_string(&receipt)
        .unwrap_or_else(|_| panic!("no receipt:\n{}", common::text(&output)));
    (output, serde_json::from_str(&text).unwrap())
}

/// What every fixture asserts: the run ends, the routing replays, reviewers wrote nothing,
/// the model calls stay within the cap, and the fixture's tests pass.
fn check_common(
    provider: &str,
    ws: &Path,
    home: &Path,
    output: &std::process::Output,
    receipt: &Value,
) {
    let shown = common::text(output);
    println!("--- {provider} ---\n{shown}");
    println!("routed: {:#}", receipt["routed"]);
    println!("finished: {:#}", receipt["finished"]);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert_eq!(receipt["chainVerified"], true);
    let routed = &receipt["routed"];
    let plan = adk_cli::harness::ultracode::replay(routed).expect("the routing replays");
    assert_eq!(serde_json::to_value(&plan).unwrap(), routed["plan"]);
    if routed["rule"] == "heuristic" && routed["inputs"]["limits"]["router"] == "model" {
        assert!(routed["fallback"]["code"].is_string(), "a fallback is recorded: {routed:#}");
    }
    let reviewed = receipt["reviewed"].as_array().unwrap();
    assert!(!reviewed.is_empty(), "at least one review stage ran");
    for stage in reviewed {
        assert_eq!(stage["workspaceUnchanged"], true, "reviewers wrote nothing: {stage:#}");
    }
    let calls = receipt["finished"]["modelCalls"].as_u64().unwrap();
    let cap = receipt["started"]["limits"]["maxModelCalls"].as_u64().unwrap();
    assert!(calls <= cap, "{calls} model calls within {cap}");
    let test = Command::new("cargo")
        .args(["test", "--quiet", "--offline"])
        .current_dir(ws)
        .env("CARGO_TARGET_DIR", home.join("target"))
        .output()
        .expect("cargo runs");
    assert!(test.status.success(), "the fixture's tests pass:\n{}", common::text(&test));
}

/// Writes the fixture's `Cargo.lock` up front: the implementer's `cargo test` would create
/// it, and a new root `Cargo.lock` is a protected path, which routes to the board.
fn lock(ws: &Path) {
    let status = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "--quiet"])
        .current_dir(ws)
        .status()
        .expect("cargo runs");
    assert!(status.success());
}

fn crate_fixture(ws: &Path, name: &str) {
    write(
        ws,
        "Cargo.toml",
        &format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n"
        ),
    );
}

#[test]
#[ignore = "live: needs ANTHROPIC_API_KEY or GOOGLE_API_KEY"]
fn live_ultracode_off_by_one() {
    for provider in providers() {
        let home = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        crate_fixture(ws.path(), "lastitem");
        write(
            ws.path(),
            "src/lib.rs",
            "/// The last element of `items`.\npub fn last(items: &[i32]) -> Option<i32> {\n    if items.is_empty() {\n        None\n    } else {\n        Some(items[items.len() - 2])\n    }\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn last_is_the_final_element() {\n        assert_eq!(super::last(&[1, 2, 3]), Some(3));\n        assert_eq!(super::last(&[]), None);\n    }\n}\n",
        );
        lock(ws.path());
        let (output, receipt) = run(
            &provider,
            ws.path(),
            home.path(),
            "Fix the off-by-one bug in last() in src/lib.rs so that `cargo test` passes. Keep the change minimal.",
        );
        let routed = &receipt["routed"];
        assert!(
            ["graph", "blackboard"].contains(&routed["plan"]["architecture"].as_str().unwrap())
        );
        check_common(provider.0, ws.path(), home.path(), &output, &receipt);
    }
}

#[test]
#[ignore = "live: needs ANTHROPIC_API_KEY or GOOGLE_API_KEY"]
fn live_ultracode_token_expiry_goes_to_a_board() {
    for provider in providers() {
        let home = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        crate_fixture(ws.path(), "sessions");
        write(ws.path(), "src/lib.rs", "pub mod auth;\n");
        write(ws.path(), "src/auth/mod.rs", "pub mod session;\n");
        write(
            ws.path(),
            "src/auth/session.rs",
            "/// A login session.\npub struct Session {\n    pub issued_at: u64,\n}\n\nimpl Session {\n    /// Whether the session is still valid at `now` (seconds).\n    pub fn is_valid(&self, now: u64) -> bool {\n        let _ = now;\n        true\n    }\n}\n",
        );
        lock(ws.path());
        let (output, receipt) = run(
            &provider,
            ws.path(),
            home.path(),
            "Make Session::is_valid in src/auth/session.rs return false once the session is older than one hour (3600 seconds), and add a unit test for it.",
        );
        let routed = &receipt["routed"];
        assert_eq!(routed["rule"], "sensitive", "{routed:#}");
        assert_eq!(routed["plan"]["architecture"], "blackboard");
        assert!(routed["plan"]["reviewers"].as_array().unwrap().contains(&"security".into()));
        for stage in receipt["reviewed"].as_array().unwrap() {
            for verdict in stage["verdicts"].as_array().unwrap() {
                let focus = verdict["focus"].as_str().unwrap();
                let fell_back =
                    stage["fallback"].as_array().is_some_and(|seats| seats.contains(&focus.into()));
                assert!(
                    verdict["verdict"] != "failed" || fell_back || stage["status"] != "completed",
                    "{stage:#}"
                );
            }
        }
        check_common(provider.0, ws.path(), home.path(), &output, &receipt);
    }
}

#[test]
#[ignore = "live: needs ANTHROPIC_API_KEY or GOOGLE_API_KEY"]
fn live_ultracode_trait_rename_across_crates() {
    for provider in providers() {
        let home = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        write(
            ws.path(),
            "Cargo.toml",
            "[workspace]\nmembers = [\"greet\", \"app\"]\nresolver = \"2\"\n",
        );
        write(
            ws.path(),
            "greet/Cargo.toml",
            "[package]\nname = \"greet\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write(
            ws.path(),
            "greet/src/lib.rs",
            "/// Says hello.\npub trait Hello {\n    fn hello(&self) -> String;\n}\n\npub struct English;\n\nimpl Hello for English {\n    fn hello(&self) -> String {\n        \"hello\".to_string()\n    }\n}\n",
        );
        write(
            ws.path(),
            "app/Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ngreet = { path = \"../greet\" }\n",
        );
        write(
            ws.path(),
            "app/src/lib.rs",
            "use greet::Hello;\n\npub fn greet_all(items: &[&dyn Hello]) -> Vec<String> {\n    items.iter().map(|item| item.hello()).collect()\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn greets() {\n        assert_eq!(super::greet_all(&[&greet::English]), [\"hello\"]);\n    }\n}\n",
        );
        lock(ws.path());
        let (output, receipt) = run(
            &provider,
            ws.path(),
            home.path(),
            "Rename the trait Hello to Greeter across both crates (greet and app), updating every use, so that `cargo test` still passes.",
        );
        let routed = &receipt["routed"];
        assert!(["model", "heuristic"].contains(&routed["rule"].as_str().unwrap()), "{routed:#}");
        check_common(provider.0, ws.path(), home.path(), &output, &receipt);
    }
}

#[test]
#[ignore = "live: needs ANTHROPIC_API_KEY or GOOGLE_API_KEY"]
fn live_ultracode_a_revise_escalates_the_review() {
    for provider in providers() {
        let home = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        crate_fixture(ws.path(), "pages");
        write(ws.path(), "src/lib.rs", "pub mod html;\n");
        let stub = "/// Escapes `<`, `>`, `&` and `\"` in `text` for HTML.\npub fn escape(text: &str) -> String {\n    // TODO: escape the text; it is returned unchanged for now.\n    text.to_string()\n}\n";
        write(ws.path(), "src/html/mod.rs", stub);
        // The operator marks the HTML helpers sensitive and keeps three reviewers, so the
        // routed roster has no security seat until a change reaches them.
        let config = "[ultracode]\nmax_reviewers = 3\nsensitive_paths = [\"src/html/*\"]\n";
        for dir in ["Library/Application Support/adk-rust", ".config/adk-rust"] {
            write(home.path(), &format!("{dir}/harness.toml"), config);
        }
        lock(ws.path());
        let (output, receipt) = run(
            &provider,
            ws.path(),
            home.path(),
            "In src/lib.rs, add `pub fn welcome(name: &str) -> String` that returns `format!(\"<p>Welcome, {}</p>\", html::escape(name))`, with a unit test in src/lib.rs that `welcome(\"<b>Ann</b>\")` is `\"<p>Welcome, &lt;b&gt;Ann&lt;/b&gt;</p>\"`. Change only src/lib.rs in this first step; code review asks for any follow-up change.",
        );
        let routed = &receipt["routed"];
        let escalated = &receipt["escalated"];
        println!("escalated: {escalated:#}");
        let html_changed =
            std::fs::read_to_string(ws.path().join("src/html/mod.rs")).unwrap() != stub;
        if routed["rule"] == "sensitive" {
            println!("{}: the first turn changed src/html; no escalation to show", provider.0);
        } else if html_changed {
            assert_eq!(
                routed["plan"]["reviewers"],
                serde_json::json!(["correctness", "edge-cases", "style"])
            );
            assert_eq!(
                escalated["change"]["sensitiveMatched"],
                serde_json::json!(["src/html/mod.rs"])
            );
            let plan = adk_cli::harness::ultracode::replay_escalation(routed, escalated)
                .expect("the escalation replays");
            assert_eq!(serde_json::to_value(&plan).unwrap(), escalated["plan"]);
            assert_eq!(
                escalated["plan"]["reviewers"],
                serde_json::json!(["correctness", "edge-cases", "security"])
            );
            let round = usize::try_from(escalated["round"].as_u64().unwrap()).unwrap();
            let next = &receipt["reviewed"][round];
            let focuses: Vec<&str> = next["verdicts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|verdict| verdict["focus"].as_str().unwrap())
                .collect();
            assert_eq!(focuses, ["correctness", "edge-cases", "security"], "{next:#}");
            assert_eq!(receipt["finished"]["escalated"], true);
        } else {
            assert!(escalated.is_null(), "{escalated:#}");
        }
        check_common(provider.0, ws.path(), home.path(), &output, &receipt);
    }
}
