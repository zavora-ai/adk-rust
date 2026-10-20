//! Sets `cfg(harness_fact_store)` when the `adk-harness` this crate builds against exports
//! `FactStore` (spec 3.1 §7.15.2), so the long-term memory scenario knows whether it can run.

use std::path::Path;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(harness_fact_store)");
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let lib = Path::new(&manifest).join("../../adk-harness/src/lib.rs");
    println!("cargo::rerun-if-changed={}", lib.display());
    let exported = std::fs::read_to_string(&lib).is_ok_and(|source| {
        source
            .lines()
            .any(|line| !line.trim_start().starts_with("//") && line.contains("FactStore"))
    });
    if exported {
        println!("cargo::rustc-cfg=harness_fact_store");
    }
}
