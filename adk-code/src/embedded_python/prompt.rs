//! Prompt-snippet rendering for the Monty executors.
//!
//! Both executor products describe their **built** capabilities through
//! [`CodeExecutor::prompt_snippet`](crate::CodeExecutor::prompt_snippet) —
//! grants and the host-function registry are immutable after `build_*()`, so
//! the snippet is rendered once and cached in the shared core. Because the
//! snippet and the in-interpreter behavior derive from the same configuration,
//! they cannot drift.
//!
//! Environment variable **names only** are rendered — values never appear in
//! the prompt.

use super::host_fn::FunctionRegistry;
use super::os_access::OsAccess;

/// The state-persistence wording that differs between the two products.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModeWording {
    /// Fresh interpreter per call.
    OneShot,
    /// Interpreter state persists across calls.
    Repl,
}

/// The exact `pathlib.Path` surface Monty implements, listed for the model
/// when any path is mounted (anything else raises `AttributeError`).
///
/// Hand-maintained against the `monty` release pinned in `Cargo.toml`
/// (0.0.21) — re-check this list whenever that pin is bumped, or the prompt
/// silently drifts from interpreter behavior. Public so other Monty
/// integrations (`adk-codeact-monty`) render the same list from the same
/// source.
pub const SUPPORTED_PATH_METHODS: &str = "  Monty implements only this subset of `pathlib.Path` (any other method raises AttributeError):
    - Read/query (any mount): `exists()`, `is_file()`, `is_dir()`, `is_symlink()`, `read_text()`, `read_bytes()`, `stat()`, `iterdir()`, `resolve()`, `absolute()`, `open(\"r\")`.
    - Write (read-write mounts only): `write_text(s)`, `write_bytes(b)`, `append_text(s)`, `append_bytes(b)`, `mkdir(parents=False, exist_ok=False)`, `unlink()`, `rmdir()`, `rename(target)`, `open(\"w\")`/`open(\"a\")`.
    - Pure path ops (no I/O, always available): the `/` operator and `joinpath(...)`, `is_absolute()`, `with_name()`, `with_stem()`, `with_suffix()`, `as_posix()`, and the properties `.name`, `.parent`, `.stem`, `.suffix`, `.suffixes`, `.parts`.
";

/// Render the full snippet from the built grants and registry.
pub(crate) fn render_prompt_snippet(
    os: &OsAccess,
    registry: &FunctionRegistry,
    mode: ModeWording,
) -> String {
    let mut snippet = String::from("## Python execution environment\n");

    match mode {
        ModeWording::Repl => snippet.push_str(
            "- State: persistent REPL session — variables, functions, and imports persist \
             across calls. Pass `reset: true` to start fresh.\n",
        ),
        ModeWording::OneShot => snippet.push_str(
            "- State: each call runs in a fresh interpreter — no state persists between calls.\n",
        ),
    }

    if os.mounts.is_empty() {
        snippet.push_str("- Filesystem: no access. Any path access raises OSError.\n");
    } else {
        let mounts: Vec<String> = os
            .mounts
            .iter()
            .map(|spec| format!("{} ({})", spec.virtual_path, spec.access.label()))
            .collect();
        snippet.push_str(&format!(
            "- Filesystem: use `pathlib.Path` against these paths only: {}. \
             Any other path raises OSError (existence checks return False).\n",
            mounts.join(", ")
        ));
        snippet.push_str(SUPPORTED_PATH_METHODS);
    }

    if os.environ.is_empty() {
        snippet.push_str("- Environment variables: none available.\n");
    } else {
        let names: Vec<&str> = os.environ.keys().map(String::as_str).collect();
        snippet.push_str(&format!(
            "- Environment variables: {} (via os.getenv / os.environ).\n",
            names.join(", ")
        ));
    }

    if os.system_clock {
        snippet
            .push_str("- Clock: real system time available via datetime.now() / date.today().\n");
    } else {
        snippet.push_str(
            "- Clock: not available — calling datetime.now() or date.today() raises OSError.\n",
        );
    }

    snippet.push_str("- Network and subprocess access: unavailable.\n");
    snippet.push_str(
        "- The value of the final expression is returned as structured output; print() \
         output is captured as stdout.\n",
    );

    if !registry.is_empty() {
        snippet.push_str(
            "- Available functions (call synchronously — never use `await`):\n\n```python\n",
        );
        for function in registry.iter() {
            snippet.push_str(&format!(
                "def {}:\n    \"\"\"{}\"\"\"\n\n",
                function.signature(),
                function.description()
            ));
        }
        // Drop the trailing blank line inside the fence.
        snippet.truncate(snippet.trim_end_matches('\n').len());
        snippet.push_str("\n```\n");
    }

    snippet
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::super::host_fn::{ClosureHostFunction, HostFunction, HostFunctionError};
    use super::super::os_access::{MountSpec, PathAccess};
    use super::*;

    use async_trait::async_trait;
    use serde_json::{Map, Value, json};

    struct GetWeather;

    #[async_trait]
    impl HostFunction for GetWeather {
        fn name(&self) -> &str {
            "get_weather"
        }
        fn description(&self) -> &str {
            "Current weather for a city."
        }
        fn signature(&self) -> String {
            "get_weather(city: str, unit: str = \"C\") -> dict".to_string()
        }
        async fn call(
            &self,
            _args: Vec<Value>,
            _kwargs: Map<String, Value>,
        ) -> Result<Value, HostFunctionError> {
            Ok(json!({}))
        }
    }

    fn full_access() -> OsAccess {
        OsAccess {
            mounts: vec![
                MountSpec {
                    virtual_path: "/data".to_string(),
                    host_path: PathBuf::from("/srv/data"),
                    access: PathAccess::ReadOnly,
                },
                MountSpec {
                    virtual_path: "/out".to_string(),
                    host_path: PathBuf::from("/srv/out"),
                    access: PathAccess::ReadWrite,
                },
            ],
            environ: BTreeMap::from([
                ("PROJECT".to_string(), "acme-secret-value".to_string()),
                ("REGION".to_string(), "eu-west-1".to_string()),
            ]),
            system_clock: true,
        }
    }

    fn registry() -> FunctionRegistry {
        FunctionRegistry::build(vec![
            Arc::new(GetWeather),
            Arc::new(ClosureHostFunction::new(
                "row_count",
                "Count rows in the loaded dataset.",
                |_args, _kwargs| async move { Ok(json!(0)) },
            )),
        ])
        .unwrap()
    }

    #[test]
    fn snippet_lists_mounts_with_access_levels() {
        let snippet = render_prompt_snippet(&full_access(), &registry(), ModeWording::OneShot);
        assert!(snippet.contains("/data (read-only)"));
        assert!(snippet.contains("/out (read-write)"));
    }

    #[test]
    fn snippet_lists_environ_names_but_never_values() {
        let snippet = render_prompt_snippet(&full_access(), &registry(), ModeWording::OneShot);
        assert!(snippet.contains("PROJECT"));
        assert!(snippet.contains("REGION"));
        assert!(!snippet.contains("acme-secret-value"));
        assert!(!snippet.contains("eu-west-1"));
    }

    #[test]
    fn clock_line_tracks_the_grant() {
        let granted = render_prompt_snippet(&full_access(), &registry(), ModeWording::OneShot);
        assert!(granted.contains("real system time available"));

        let denied = render_prompt_snippet(
            &OsAccess { system_clock: false, ..full_access() },
            &registry(),
            ModeWording::OneShot,
        );
        assert!(denied.contains("Clock: not available"));
    }

    #[test]
    fn mode_wording_differs_between_products() {
        let one_shot = render_prompt_snippet(&full_access(), &registry(), ModeWording::OneShot);
        let repl = render_prompt_snippet(&full_access(), &registry(), ModeWording::Repl);
        assert!(one_shot.contains("fresh interpreter"));
        assert!(repl.contains("persistent REPL session"));
        assert!(repl.contains("reset: true"));
        assert_ne!(one_shot, repl);
    }

    #[test]
    fn function_stubs_render_signature_and_description() {
        let snippet = render_prompt_snippet(&full_access(), &registry(), ModeWording::Repl);
        assert!(snippet.contains("def get_weather(city: str, unit: str = \"C\") -> dict:"));
        assert!(snippet.contains("\"\"\"Current weather for a city.\"\"\""));
        // Default signature rendering for the closure adapter.
        assert!(snippet.contains("def row_count(...):"));
        assert!(snippet.contains("\"\"\"Count rows in the loaded dataset.\"\"\""));
    }

    #[test]
    fn fully_sandboxed_default_renders_the_no_access_lines() {
        let snippet = render_prompt_snippet(
            &OsAccess::default(),
            &FunctionRegistry::default(),
            ModeWording::OneShot,
        );
        assert!(snippet.contains("Filesystem: no access"));
        assert!(snippet.contains("Environment variables: none available"));
        assert!(snippet.contains("Clock: not available"));
        assert!(!snippet.contains("Available functions"));
    }

    #[test]
    fn pathlib_surface_renders_only_when_a_path_is_mounted() {
        let with_mounts = render_prompt_snippet(&full_access(), &registry(), ModeWording::OneShot);
        assert!(with_mounts.contains("subset of `pathlib.Path`"));

        let no_mounts = render_prompt_snippet(
            &OsAccess { mounts: Vec::new(), ..full_access() },
            &registry(),
            ModeWording::OneShot,
        );
        assert!(!no_mounts.contains("pathlib.Path"));
    }
}
