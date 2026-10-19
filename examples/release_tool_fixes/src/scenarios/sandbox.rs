//! Item 5: under Seatbelt, `python3` found through a `PATH` that starts with `/usr/bin` runs.

use std::collections::HashMap;
use std::time::Duration;

use adk_sandbox::{
    ExecRequest, Language, ProcessBackend, ProcessConfig, SandboxBackend, SandboxPolicyBuilder,
    get_enforcer,
};

use crate::common::{Mode, Report, Verdict, skip, verdict};

pub async fn run(report: &mut Report) {
    if cfg!(target_os = "macos") && std::path::Path::new("/usr/bin/python3").exists() {
        report.record_result("seatbelt-usr-bin-python3", 5, Mode::Local, usr_bin_python().await);
    } else {
        report.record(
            "seatbelt-usr-bin-python3",
            5,
            Mode::Local,
            skip("needs macOS with /usr/bin/python3"),
        );
    }
}

/// The policy's `PATH` resolves `python3` to `/usr/bin/python3`, the Xcode shim.
async fn usr_bin_python() -> anyhow::Result<Verdict> {
    let policy = SandboxPolicyBuilder::new().env("PATH", "/usr/bin:/usr/local/bin").build();
    let backend = ProcessBackend::with_sandbox(ProcessConfig::default(), get_enforcer()?, policy);
    let result = backend
        .execute(ExecRequest {
            language: Language::Python,
            code: "import sys\nprint(1 + 1)\nprint(sys.executable)".to_string(),
            stdin: None,
            timeout: Duration::from_secs(60),
            memory_limit_mb: None,
            env: HashMap::new(),
        })
        .await?;
    let first = result.stdout.lines().next().unwrap_or_default().trim().to_string();
    Ok(verdict(vec![
        (result.exit_code == 0, format!("exit code {}", result.exit_code)),
        (first == "2", format!("stdout {:?}", result.stdout.trim())),
        (
            result.stderr.trim().is_empty(),
            format!("stderr {:?}", crate::common::short(result.stderr.trim(), 200)),
        ),
    ]))
}
