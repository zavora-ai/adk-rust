//! Where `adk-rust` runs web search (spec 3.1 §7.20, §8.1): the banner's `Search` line and
//! `/web` name the path, a search provider in the user config replaces the model provider's
//! built-in search, and `--search off` and `--no-web` turn it off. The search filter is
//! `[web] search_domains`, never the fetch allowlist, and the banner names the embedder of
//! long-term facts in every session (spec 3.1 §7.32, I73).

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

/// An OpenAI session's banner: no request is sent, the path is decided from the client.
fn openai_banner(home: &Path, ws: &Path, flags: &[&str]) -> Terminal {
    let dir = ws.display().to_string();
    let mut args = vec![
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
    args.extend_from_slice(flags);
    let mut terminal = Terminal::spawn_in(&repl(&args), home, ws);
    terminal.expect("Search    : ");
    terminal
}

/// spec 3.1 §7.32, I73: the fetch allowlist never filters search; `[web] search_domains`
/// does, and the workspace's file may only narrow it.
#[test]
fn the_banner_names_the_search_filter_never_the_fetch_allowlist() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();

    let mut terminal =
        openai_banner(home.path(), ws.path(), &["--allow-domain", "internal.example"]);
    terminal.expect("openai openai_web_search, run by the provider: every domain");
    terminal.expect("You >");
    terminal.send("/web\r");
    terminal.expect("Allowlist (no prompt): internal.example");
    terminal.expect("Search: openai openai_web_search, run by the provider: every domain");
    terminal.finish();

    user_config(
        home.path(),
        "[web]\nsearch_domains = { allowed = [\"docs.rs\", \"rust-lang.org\"] }\n",
    );
    let mut terminal = openai_banner(home.path(), ws.path(), &[]);
    terminal.expect("openai openai_web_search, run by the provider: only docs.rs, rust-lang.org");
    terminal.finish();

    // The workspace cannot widen the filter: what it adds is dropped, here everything.
    let project = ws.path().join(".adk");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("harness.toml"),
        "[web]\nsearch_domains = { allowed = [\"collector.example\"] }\n",
    )
    .unwrap();
    let mut terminal = openai_banner(home.path(), ws.path(), &[]);
    terminal.expect("off (search_domains allows no domain, so no search is declared)");
    terminal.finish();
}

/// spec 3.1 §7.32, §13 Q9: an Anthropic session embeds facts with the local model or not at
/// all, whatever other providers' keys are in the environment; another provider only when
/// chosen. The hub endpoint is unreachable, so the local model is unavailable here.
#[test]
fn an_anthropic_session_never_embeds_with_another_providers_key() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let dir = ws.path().display().to_string();
    let hf = home.path().join("hf").display().to_string();
    let env = [
        ("GOOGLE_API_KEY", "test-google-key-not-sent"),
        ("OPENAI_API_KEY", "test-openai-key-not-sent"),
        ("HF_HOME", hf.as_str()),
        ("HF_ENDPOINT", "http://127.0.0.1:9"),
    ];
    let anthropic = |flags: &[&'static str]| {
        let mut args = vec![
            "--dir",
            dir.as_str(),
            "--provider",
            "anthropic",
            "--model",
            "claude-sonnet-5-5",
            "--api-key",
            "sk-ant-test-not-sent",
        ];
        args.extend_from_slice(flags);
        Terminal::spawn_sized_env(&repl(&args), home.path(), ws.path(), 50, 220, &env)
    };

    let mut terminal = anthropic(&[]);
    terminal.expect("the local embedding model is unavailable");
    terminal
        .expect("Embedder  : none: facts are found by shared words; nothing leaves this machine");
    terminal.expect("You >");
    let text = terminal.finish();
    assert!(!text.contains("Gemini") && !text.contains("OpenAI ("), "{text}");

    let mut terminal = anthropic(&["--embedder", "gemini"]);
    terminal.expect("Embedder  : Gemini (gemini-embedding-2)");
    terminal.finish();
}
