//! `scripts/check-cargo-adk-templates.sh` compiles a project for every template, pattern and
//! addon `cargo adk` lists, so a scaffold that stops compiling fails CI.

use std::collections::BTreeSet;
use std::path::Path;

use cargo_adk::registry::TemplateRegistry;

/// The names in the JSON listing `cargo adk <command> --json` prints.
fn listed_names(command: &str) -> Vec<String> {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cargo-adk"))
        .args(["adk", command, "--json"])
        .output()
        .expect("cargo-adk runs");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let listing: Vec<serde_json::Value> =
        serde_json::from_slice(&output.stdout).expect("the listing is a JSON array");
    listing.iter().map(|item| item["name"].as_str().expect("a name").to_string()).collect()
}

/// The `template:provider[:addons]` entries of the check script's `templates` array.
fn script_entries() -> Vec<(String, Vec<String>)> {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/check-cargo-adk-templates.sh");
    let script = std::fs::read_to_string(&script).expect("the check script is readable");
    let array = script
        .split_once("templates=(\n")
        .and_then(|(_, rest)| rest.split_once("\n)"))
        .map(|(array, _)| array)
        .expect("the script has a templates array");
    array
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix('"')?.strip_suffix('"'))
        .map(|entry| {
            let mut fields = entry.split(':');
            let template = fields.next().unwrap_or_default().to_string();
            let addons = fields
                .nth(1)
                .map(|addons| addons.split(',').map(str::to_string).collect())
                .unwrap_or_default();
            (template, addons)
        })
        .collect()
}

#[test]
fn every_template_pattern_and_addon_has_a_compile_check() {
    let registry = TemplateRegistry::builtin();
    let canonical = |name: &str| registry.aliases.get(name).copied().unwrap_or(name).to_string();
    let entries = script_entries();

    let checked: BTreeSet<String> =
        entries.iter().map(|(template, _)| canonical(template)).collect();
    let unchecked: Vec<String> = listed_names("templates")
        .iter()
        .map(|name| canonical(name))
        .filter(|name| !checked.contains(name))
        .collect();
    assert!(unchecked.is_empty(), "templates without a compile check: {unchecked:?}");

    let checked: BTreeSet<&String> = entries.iter().flat_map(|(_, addons)| addons).collect();
    let unchecked: Vec<String> =
        listed_names("addons").into_iter().filter(|name| !checked.contains(name)).collect();
    assert!(unchecked.is_empty(), "addons without a compile check: {unchecked:?}");
}
