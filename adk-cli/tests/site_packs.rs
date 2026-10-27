//! `adk-rust site packs` (spec 3.5 §7.23): a publisher signs a pack, the owner trusts the
//! publisher's key, installs, lists, upgrades and removes it; an unsigned pack, a pack from a
//! publisher the site does not trust, a downgrade and a built-in pack's removal are refused.

#![cfg(feature = "website")]

use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};

fn adk_rust(args: &[&str], site: &Path) -> Output {
    let mut all: Vec<String> = args.iter().map(ToString::to_string).collect();
    all.extend(["--site".to_string(), site.display().to_string()]);
    Command::new(env!("CARGO_BIN_EXE_adk-rust"))
        .args(&all)
        .env("HOME", site)
        .env("XDG_CONFIG_HOME", site.join(".config"))
        .env("RUST_LOG", "off")
        .stdin(std::process::Stdio::null())
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

#[track_caller]
fn ok(output: Output) -> String {
    let printed = text(&output);
    assert!(output.status.success(), "{printed}");
    printed
}

#[track_caller]
fn refused(output: Output, needle: &str) {
    let printed = text(&output);
    assert!(!output.status.success(), "{printed}");
    assert!(printed.contains(needle), "{needle}: {printed}");
}

/// An unsigned bundle of the menu pack at `version`.
fn unsigned(version: &str) -> String {
    json!({
        "format": "adk-website.pack/1",
        "name": "menu",
        "version": version,
        "publisher": { "id": "tables.example", "name": "Tables" },
        "description": "Tonight's menu at its published prices",
        "capabilities": [{
            "name": "list_menu",
            "description": "Tonight's menu",
            "effect": "read_only",
            "minTrust": "public",
            "approval": { "mode": "never" },
            "idempotency": "binding"
        }],
        "handlers": { "list_menu": { "handler": "listing", "content": "product" } },
        "drivers": [{
            "id": "tables-pos",
            "manifest": "id = \"tables-pos\"\ncommand = \"mcp-tables-pos\"\npartitions = [\"back-office\"]\n\n[tools]\ntables_list = \"read\"\n"
        }]
    })
    .to_string()
}

#[test]
fn a_publisher_signs_and_the_owner_trusts_installs_upgrades_and_removes() {
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path();
    let work = tempfile::tempdir().unwrap();
    let write = |name: &str, contents: &str| {
        let path = work.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path.display().to_string()
    };
    // The publisher's private key: an Ed25519 JWK whose `d` is the seed.
    let key = write(
        "publisher.jwk",
        r#"{"kty":"OKP","crv":"Ed25519","kid":"tables-2026","d":"VFRUVFRUVFRUVFRUVFRUVFRUVFRUVFRUVFRUVFRUVFQ"}"#,
    );
    let unsigned_one = write("menu.json", &unsigned("1.0.0"));
    refused(adk_rust(&["site", "packs", "install", &unsigned_one, "--yes"], site), "not signed");

    let signed = ok(adk_rust(&["site", "packs", "sign", &unsigned_one, "--key", &key], site));
    let public = signed.lines().last().unwrap();
    let public_key: Value = serde_json::from_str(public).unwrap();
    assert_eq!(public_key["kid"], "tables-2026");
    assert!(public_key.get("d").is_none(), "the private key is never printed");
    let one = work.path().join("menu-1.0.0.pack.json").display().to_string();

    refused(adk_rust(&["site", "packs", "install", &one, "--yes"], site), "not in the trust list");
    refused(
        adk_rust(&["site", "packs", "trust", "tables.example", &key, "--yes"], site),
        "private key",
    );
    let public_file = write("tables-key.json", public);
    let trusted =
        ok(adk_rust(&["site", "packs", "trust", "tables.example", &public_file, "--yes"], site));
    assert!(trusted.contains("thumbprint"), "{trusted}");

    let installed = ok(adk_rust(&["site", "packs", "install", &one, "--yes"], site));
    assert!(installed.contains("capability list_menu"), "{installed}");
    assert!(installed.contains("adk-rust drivers mount"), "{installed}");
    assert!(site.join("packs/menu.pack.json").is_file());
    assert!(site.join("packs/drivers/menu/tables-pos.toml").is_file());

    let listed: Value =
        serde_json::from_slice(&adk_rust(&["site", "packs", "list", "--json"], site).stdout)
            .unwrap();
    let names: Vec<&str> =
        listed["packs"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["contact", "quote", "booking", "order", "search", "components", "menu"]);
    assert_eq!(listed["packs"][6]["origin"], "installed");
    assert_eq!(listed["refused"], json!([]));

    let unsigned_two = write("menu-next.json", &unsigned("1.1.0"));
    ok(adk_rust(&["site", "packs", "sign", &unsigned_two, "--key", &key], site));
    let two = work.path().join("menu-1.1.0.pack.json").display().to_string();
    let upgraded = ok(adk_rust(&["site", "packs", "install", &two, "--yes"], site));
    assert!(upgraded.contains("Upgrades menu from 1.0.0 to 1.1.0"), "{upgraded}");
    refused(adk_rust(&["site", "packs", "install", &one, "--yes"], site), "never downgraded");

    refused(adk_rust(&["site", "packs", "remove", "booking", "--yes"], site), "built-in pack");
    let removed = ok(adk_rust(&["site", "packs", "remove", "menu", "--yes"], site));
    assert!(removed.contains("adk-rust drivers unmount tables-pos"), "{removed}");
    assert!(!site.join("packs/menu.pack.json").exists());
    assert!(!site.join("packs/drivers/menu").exists());
}
