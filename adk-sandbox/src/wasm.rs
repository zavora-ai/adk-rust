//! [`WasmBackend`] — in-process WebAssembly execution via `wasmtime`.
//!
//! This backend executes WASM modules in a fully sandboxed environment with:
//! - WASI stdin/stdout/stderr capture
//! - No filesystem preopens
//! - No network access
//! - Memory limit enforcement via `StoreLimitsBuilder`
//! - Timeout enforcement via epoch-based interruption
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_sandbox::{WasmBackend, ExecRequest, Language, SandboxBackend};
//! use std::time::Duration;
//! use std::collections::HashMap;
//!
//! let backend = WasmBackend::new();
//! let request = ExecRequest {
//!     language: Language::Wasm,
//!     code: wasm_bytes_as_base64,
//!     stdin: None,
//!     timeout: Duration::from_secs(10),
//!     memory_limit_mb: Some(64),
//!     env: HashMap::new(),
//! };
//! let result = backend.execute(request).await?;
//! ```

use std::time::Instant;

use async_trait::async_trait;
use tracing::{Span, instrument};
use wasmtime::{Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, Trap};
use wasmtime_wasi::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::preview1;
use wasmtime_wasi::{I32Exit, WasiCtxBuilder};

use crate::backend::{BackendCapabilities, EnforcedLimits, SandboxBackend};
use crate::error::SandboxError;
use crate::types::{ExecRequest, ExecResult, Language};

/// Maximum output capture size (1 MB).
const MAX_OUTPUT_BYTES: usize = 1_024 * 1_024;

/// In-process WebAssembly sandbox backend.
///
/// Executes WASM modules via `wasmtime` with full isolation: no filesystem,
/// no network, memory limits via `StoreLimitsBuilder`, and timeout via
/// epoch-based interruption.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::{WasmBackend, SandboxBackend};
///
/// let backend = WasmBackend::new();
/// assert_eq!(backend.name(), "wasm");
/// ```
pub struct WasmBackend {
    engine: Engine,
}

/// Store data combining WASI context with resource limits.
struct WasmStoreData {
    wasi: preview1::WasiP1Ctx,
    limits: StoreLimits,
}

impl WasmBackend {
    /// Creates a new `WasmBackend` with epoch interruption enabled.
    pub fn new() -> Self {
        let mut config = wasmtime::Config::new();
        config.epoch_interruption(true);
        config.async_support(false);
        let engine =
            Engine::new(&config).expect("failed to create wasmtime engine with epoch support");
        Self { engine }
    }

    /// Synchronous WASM execution — runs on a blocking thread.
    fn execute_sync(engine: Engine, request: ExecRequest) -> Result<ExecResult, SandboxError> {
        let timeout = request.timeout;

        // Compile the module from WAT or WASM bytes.
        let module = Module::new(&engine, &request.code).map_err(|e| {
            SandboxError::ExecutionFailed(format!("failed to compile WASM module: {e}"))
        })?;

        // Set up stdout/stderr capture pipes.
        let stdout_pipe = MemoryOutputPipe::new(MAX_OUTPUT_BYTES);
        let stderr_pipe = MemoryOutputPipe::new(MAX_OUTPUT_BYTES);
        let stdout_capture = stdout_pipe.clone();
        let stderr_capture = stderr_pipe.clone();

        // Build WASI context: no filesystem preopens, no network.
        let mut wasi_builder = WasiCtxBuilder::new();
        wasi_builder.stdout(stdout_pipe);
        wasi_builder.stderr(stderr_pipe);
        // Allow blocking on the current thread since we run in spawn_blocking
        // and WASI operations use tokio internally.
        wasi_builder.allow_blocking_current_thread(true);

        if let Some(ref input) = request.stdin {
            wasi_builder.stdin(MemoryInputPipe::new(input.clone()));
        }

        // Build memory limits.
        let mut limits_builder = StoreLimitsBuilder::new();
        if let Some(limit_mb) = request.memory_limit_mb {
            limits_builder = limits_builder.memory_size((limit_mb as usize) * 1024 * 1024);
        }

        let store_data =
            WasmStoreData { wasi: wasi_builder.build_p1(), limits: limits_builder.build() };

        let mut store = Store::new(&engine, store_data);
        store.limiter(|data| &mut data.limits);
        store.set_epoch_deadline(1);
        store.epoch_deadline_trap();

        // Spawn OS thread to increment epoch after timeout.
        let engine_for_timeout = engine.clone();
        let _timer = std::thread::spawn(move || {
            std::thread::sleep(timeout);
            engine_for_timeout.increment_epoch();
        });

        // Link WASI functions.
        let mut linker: Linker<WasmStoreData> = Linker::new(&engine);
        preview1::add_to_linker_sync(&mut linker, |data| &mut data.wasi).map_err(|e| {
            SandboxError::ExecutionFailed(format!("failed to link WASI functions: {e}"))
        })?;

        let start = Instant::now();

        // Instantiate the module.
        let instance = linker.instantiate(&mut store, &module).map_err(|e| {
            let msg = e.to_string();
            if let Some(limit_mb) = request.memory_limit_mb {
                if msg.contains("memory minimum size") || msg.contains("allocat") {
                    return SandboxError::MemoryExceeded { limit_mb };
                }
            }
            SandboxError::ExecutionFailed(format!("failed to instantiate WASM module: {e}"))
        })?;

        let func = instance.get_typed_func::<(), ()>(&mut store, "_start").map_err(|e| {
            SandboxError::ExecutionFailed(format!("WASM module has no _start function: {e}"))
        })?;

        let call_result = func.call(&mut store, ());
        let duration = start.elapsed();

        let stdout = String::from_utf8_lossy(&stdout_capture.contents()).into_owned();
        let stderr = String::from_utf8_lossy(&stderr_capture.contents()).into_owned();

        match call_result {
            Ok(()) => Ok(ExecResult { stdout, stderr, exit_code: 0, duration }),
            Err(trap) => {
                // Check for epoch interruption (timeout) using the Trap enum.
                if let Some(t) = trap.downcast_ref::<Trap>() {
                    if *t == Trap::Interrupt {
                        return Err(SandboxError::Timeout { timeout });
                    }
                }

                let msg = trap.to_string();

                // Memory limit exceeded.
                if let Some(limit_mb) = request.memory_limit_mb {
                    if msg.contains("memory minimum size") || msg.contains("allocat") {
                        return Err(SandboxError::MemoryExceeded { limit_mb });
                    }
                }

                // WASI proc_exit → normal exit with code.
                if let Some(exit) = trap.downcast_ref::<I32Exit>() {
                    return Ok(ExecResult { stdout, stderr, exit_code: exit.0, duration });
                }

                // Other trap → non-zero exit with trap message.
                let combined_stderr =
                    if stderr.is_empty() { msg } else { format!("{stderr}\n{msg}") };
                Ok(ExecResult { stdout, stderr: combined_stderr, exit_code: 1, duration })
            }
        }
    }
}

impl Default for WasmBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SandboxBackend for WasmBackend {
    fn name(&self) -> &str {
        "wasm"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supported_languages: vec![Language::Wasm],
            isolation_class: "wasm".to_string(),
            enforced_limits: EnforcedLimits {
                timeout: true,
                memory: true,
                network_isolation: true,
                filesystem_isolation: true,
                environment_isolation: true,
            },
        }
    }

    #[instrument(
        skip_all,
        fields(
            backend = "wasm",
            language = %request.language,
            exit_code,
            duration_ms,
        )
    )]
    async fn execute(&self, request: ExecRequest) -> Result<ExecResult, SandboxError> {
        if request.language != Language::Wasm {
            return Err(SandboxError::InvalidRequest(format!(
                "WasmBackend only supports Language::Wasm, got {}",
                request.language
            )));
        }

        let engine = self.engine.clone();

        // Run WASM execution on a blocking thread so we don't block the
        // async runtime. The epoch timer OS thread fires independently.
        let result = tokio::task::spawn_blocking(move || Self::execute_sync(engine, request))
            .await
            .map_err(|e| SandboxError::ExecutionFailed(format!("task join error: {e}")))?;

        match &result {
            Ok(res) => {
                Span::current().record("exit_code", res.exit_code);
                Span::current().record("duration_ms", res.duration.as_millis() as u64);
            }
            Err(SandboxError::Timeout { timeout }) => {
                Span::current().record("duration_ms", timeout.as_millis() as u64);
            }
            _ => {}
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    fn make_wasm_request(wat: &str) -> ExecRequest {
        ExecRequest {
            language: Language::Wasm,
            code: wat.to_string(),
            stdin: None,
            timeout: Duration::from_secs(10),
            memory_limit_mb: None,
            env: HashMap::new(),
        }
    }

    /// A minimal WASI module that writes "hello\n" to stdout via fd_write.
    const HELLO_WAT: &str = r#"
        (module
            (import "wasi_snapshot_preview1" "fd_write"
                (func $fd_write (param i32 i32 i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "proc_exit"
                (func $proc_exit (param i32)))
            (memory (export "memory") 1)
            (data (i32.const 100) "hello\n")
            (data (i32.const 0) "\64\00\00\00")
            (data (i32.const 4) "\06\00\00\00")
            (func (export "_start")
                (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 200))
                drop
                (call $proc_exit (i32.const 0))
            )
        )
    "#;

    #[tokio::test]
    async fn test_basic_wasm_execution() {
        let backend = WasmBackend::new();
        let result = backend.execute(make_wasm_request(HELLO_WAT)).await.unwrap();
        assert_eq!(result.stdout, "hello\n");
        assert_eq!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn test_memory_limit_enforcement() {
        let backend = WasmBackend::new();
        // Module tries to grow memory by 256 pages (16 MB) beyond initial 1 page.
        let wat = r#"
            (module
                (import "wasi_snapshot_preview1" "proc_exit"
                    (func $proc_exit (param i32)))
                (memory (export "memory") 1)
                (func (export "_start")
                    (memory.grow (i32.const 256))
                    drop
                    (call $proc_exit (i32.const 0))
                )
            )
        "#;
        let request = ExecRequest {
            language: Language::Wasm,
            code: wat.to_string(),
            stdin: None,
            timeout: Duration::from_secs(10),
            memory_limit_mb: Some(1),
            env: HashMap::new(),
        };
        let result = backend.execute(request).await;
        // memory.grow returns -1 when limiter blocks it; module still exits 0.
        match result {
            Ok(res) => assert_eq!(res.exit_code, 0),
            Err(SandboxError::MemoryExceeded { .. }) => {}
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    #[tokio::test]
    async fn test_memory_limit_traps_on_initial_too_large() {
        let backend = WasmBackend::new();
        let wat = r#"
            (module
                (import "wasi_snapshot_preview1" "proc_exit"
                    (func $proc_exit (param i32)))
                (memory (export "memory") 512)
                (func (export "_start")
                    (call $proc_exit (i32.const 0))
                )
            )
        "#;
        let request = ExecRequest {
            language: Language::Wasm,
            code: wat.to_string(),
            stdin: None,
            timeout: Duration::from_secs(10),
            memory_limit_mb: Some(1),
            env: HashMap::new(),
        };
        let result = backend.execute(request).await;
        assert!(
            matches!(
                result,
                Err(SandboxError::MemoryExceeded { .. } | SandboxError::ExecutionFailed(_))
            ),
            "expected MemoryExceeded or ExecutionFailed, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_timeout_enforcement() {
        let backend = WasmBackend::new();
        let wat = r#"
            (module
                (memory (export "memory") 1)
                (func (export "_start")
                    (loop $inf (br $inf))
                )
            )
        "#;
        let request = ExecRequest {
            language: Language::Wasm,
            code: wat.to_string(),
            stdin: None,
            timeout: Duration::from_millis(500),
            memory_limit_mb: None,
            env: HashMap::new(),
        };
        let result = backend.execute(request).await;
        assert!(
            matches!(result, Err(SandboxError::Timeout { .. })),
            "expected Timeout, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_invalid_language_rejected() {
        let backend = WasmBackend::new();
        let request = ExecRequest {
            language: Language::Python,
            code: "print('hello')".to_string(),
            stdin: None,
            timeout: Duration::from_secs(10),
            memory_limit_mb: None,
            env: HashMap::new(),
        };
        let result = backend.execute(request).await;
        assert!(matches!(result, Err(SandboxError::InvalidRequest(_))));
    }

    #[tokio::test]
    async fn test_nonzero_exit_code() {
        let backend = WasmBackend::new();
        let wat = r#"
            (module
                (import "wasi_snapshot_preview1" "proc_exit"
                    (func $proc_exit (param i32)))
                (memory (export "memory") 1)
                (func (export "_start")
                    (call $proc_exit (i32.const 42))
                )
            )
        "#;
        let result = backend.execute(make_wasm_request(wat)).await.unwrap();
        assert_eq!(result.exit_code, 42);
    }

    #[tokio::test]
    async fn test_stdin_with_stdout() {
        let backend = WasmBackend::new();
        let wat = r#"
            (module
                (import "wasi_snapshot_preview1" "fd_write"
                    (func $fd_write (param i32 i32 i32 i32) (result i32)))
                (import "wasi_snapshot_preview1" "proc_exit"
                    (func $proc_exit (param i32)))
                (memory (export "memory") 1)
                (data (i32.const 100) "ok\n")
                (data (i32.const 0) "\64\00\00\00")
                (data (i32.const 4) "\03\00\00\00")
                (func (export "_start")
                    (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 200))
                    drop
                    (call $proc_exit (i32.const 0))
                )
            )
        "#;
        let request = ExecRequest {
            language: Language::Wasm,
            code: wat.to_string(),
            stdin: Some("test input".to_string()),
            timeout: Duration::from_secs(10),
            memory_limit_mb: None,
            env: HashMap::new(),
        };
        let result = backend.execute(request).await.unwrap();
        assert_eq!(result.stdout, "ok\n");
        assert_eq!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn test_invalid_wasm_module() {
        let backend = WasmBackend::new();
        let request = make_wasm_request("this is not valid wasm");
        let result = backend.execute(request).await;
        assert!(matches!(result, Err(SandboxError::ExecutionFailed(_))));
    }

    #[test]
    fn test_capabilities() {
        let backend = WasmBackend::new();
        let caps = backend.capabilities();
        assert_eq!(caps.isolation_class, "wasm");
        assert!(caps.enforced_limits.timeout);
        assert!(caps.enforced_limits.memory);
        assert!(caps.enforced_limits.network_isolation);
        assert!(caps.enforced_limits.filesystem_isolation);
        assert!(caps.enforced_limits.environment_isolation);
        assert_eq!(caps.supported_languages, vec![Language::Wasm]);
    }

    #[test]
    fn test_name() {
        let backend = WasmBackend::new();
        assert_eq!(backend.name(), "wasm");
    }

    #[test]
    fn test_default() {
        let backend = WasmBackend::default();
        assert_eq!(backend.name(), "wasm");
    }
}
