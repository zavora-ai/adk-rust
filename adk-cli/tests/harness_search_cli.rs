//! Where `adk-rust` runs web search (spec 3.1 §7.20, §8.1): the banner's `Search` line and
//! `/web` name the path, a search provider in the user config replaces the model provider's
//! built-in search, and `--search off` and `--no-web` turn it off.

#![cfg(all(unix, feature = "scripted-model", feature = "harness", feature = "persist"))]

mod common;

use std::path::Path;

use common::{Terminal, repl};

/// Writes the user config where `adk-rust` reads it on Linux and on macOS.
fn user_config(home: &Path, text: &str) {
    for dir in [home.join(".config/adk-rust"), home.join("Library/Application Support/adk-rust")] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("harness.toml"), text).unwrap();
    }
}

fn banner_search(home: &Path, ws: &Path, flags: &[&str]) -> Terminal {
    let dir = ws.display().to_string();
    let mut args = vec!["--dir", dir.as_str(), "--ephemeral", "--model-script", "[]"];
    args.extend_from_slice(flags);
    let mut terminal = Terminal::spawn_in(&repl(&args), home, ws);
    terminal.expect("Search    : ");
    terminal
}

#[test]
fn the_banner_and_web_name_where_search_runs() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();

    // The scripted model's client serves no built-in search, so there is none, and why.
    let mut terminal = banner_search(home.path(), ws.path(), &[]);
    terminal.expect("off (the client of");
    terminal.expect("serves no provider-native search; configure a search provider");
    terminal.expect("You >");
    terminal.send("/web\r");
    terminal.expect("Search: off (the client of");
    terminal.finish();

    let mut terminal = banner_search(home.path(), ws.path(), &["--search", "off"]);
    terminal.expect("off (provider search is off and no search provider is configured)");
    terminal.finish();

    let mut terminal = banner_search(home.path(), ws.path(), &["--no-web"]);
    terminal.expect("off (web is off)");
    terminal.finish();

    // A search provider in the user config replaces the provider's search.
    user_config(
        home.path(),
        "[web]\nsearch = \"searxng\"\nsearxng_url = \"http://localhost:8888\"\n",
    );
    let mut terminal = banner_search(home.path(), ws.path(), &[]);
    terminal.expect("web_search through searxng (governed)");
    terminal.expect("You >");
    terminal.send("/web\r");
    terminal.expect("Search: web_search through searxng (governed)");
    terminal.finish();

    // The flag wins over the user config.
    let mut terminal = banner_search(home.path(), ws.path(), &["--search", "off"]);
    terminal.expect("off (provider search is off and no search provider is configured)");
    terminal.finish();
}

#[test]
fn an_openai_session_searches_with_openai_by_default() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let dir = ws.path().display().to_string();
    // No request is sent: the banner is decided from the client the CLI builds.
    let args = [
        "--dir",
        dir.as_str(),
        "--ephemeral",
        "--provider",
        "openai",
        "--model",
        "gpt-5.6-terra",
        "--api-key",
        "sk-test-not-sent",
    ];
    let mut terminal = Terminal::spawn_in(&repl(&args), home.path(), ws.path());
    terminal.expect("Search    : ");
    terminal.expect("openai openai_web_search, run by the provider: every domain");
    terminal.expect("You >");
    terminal.send("/web\r");
    terminal.expect("Search: openai openai_web_search, run by the provider");
    terminal.finish();
}
