//! `cargo adk bench` is behind the `bench` feature: a build without it names the install
//! command instead of running a benchmark.

use std::process::Output;

fn cargo_adk_bench_dry_run() -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_cargo-adk"))
        .args(["adk", "bench", "--dry-run"])
        .env_remove("RUST_LOG")
        .output()
        .expect("cargo-adk runs")
}

#[cfg(not(feature = "bench"))]
#[test]
fn bench_without_the_feature_exits_with_the_install_hint() {
    let output = cargo_adk_bench_dry_run();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert_eq!(
        stderr.trim_end(),
        "Error: cargo adk bench is not in this build; install it with \
         `cargo install cargo-adk --features bench`"
    );
    assert!(output.stdout.is_empty());
}

#[cfg(feature = "bench")]
#[test]
fn bench_with_the_feature_estimates_the_cost_without_calling_a_model() {
    let output = cargo_adk_bench_dry_run();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.contains("dry-run: displaying estimated cost without executing"), "{stderr}");
}
