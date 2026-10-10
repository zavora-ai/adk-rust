//! Smoke tests asserting the umbrella's feature forwards wire their
//! re-exported surface.
//!
//! The PR-tier `feature-coverage` matrix compiles and tests the umbrella with
//! its code-execution opt-ins (`code-tools`, `code-embedded-js`,
//! `code-embedded-python`, `code-docker`, `codeact-monty`) and with the
//! agent capability features (`guardrail`, `plugin`, `skills`). These tests
//! give those entries a real signal: a feature that stops forwarding — or a
//! re-export module that stops compiling — fails here, in the umbrella, rather
//! than in a downstream consumer.

/// Always present: the `adk-core` root re-export needs no feature.
#[test]
fn core_reexports_are_wired() {
    let content = adk_rust::Content::new("user").with_text("hi");
    assert_eq!(content.role, "user");
}

#[cfg(feature = "code-tools")]
mod code_tools {
    use adk_rust::Tool;

    /// `code-tools` turns on `adk-tool/code`, so the language-preset tools
    /// exist under `adk_rust::tool` and construct with their documented names.
    #[test]
    fn code_execution_tools_are_reachable() {
        assert_eq!(adk_rust::tool::PythonCodeTool::new().name(), "python_code");
        assert_eq!(adk_rust::tool::JavaScriptCodeTool::new().name(), "javascript_code");
        assert_eq!(adk_rust::tool::MontyPythonCodeTool::new().name(), "monty_python_code");
        assert_eq!(adk_rust::tool::FrontendCodeTool::react().name(), "frontend_code");
    }
}

#[cfg(feature = "code-embedded-js")]
mod code_embedded_js {
    use adk_rust::code::{CodeExecutor, EmbeddedJsExecutor, ExecutionLanguage};

    /// `code-embedded-js` lights up the live executor in `adk_rust::code`.
    #[test]
    fn embedded_js_executor_is_reachable() {
        let executor = EmbeddedJsExecutor::new();
        assert!(executor.supports_language(&ExecutionLanguage::JavaScript));
    }
}

#[cfg(feature = "code-embedded-python")]
mod code_embedded_python {
    use adk_rust::code::{CodeExecutor, ExecutionLanguage, MontyExecutorBuilder};

    /// `code-embedded-python` lights up the Monty executors in
    /// `adk_rust::code` (the `MontyPythonCodeTool` is covered by the
    /// `code-tools` test above).
    #[test]
    fn monty_executors_are_reachable() {
        let executor = MontyExecutorBuilder::new().build_one_shot().unwrap();
        assert!(executor.supports_language(&ExecutionLanguage::Python));
        let repl = MontyExecutorBuilder::new().build_repl().unwrap();
        assert!(repl.supports_language(&ExecutionLanguage::Python));
    }
}

#[cfg(feature = "code-docker")]
mod code_docker {
    /// `code-docker` lights up the persistent Docker executor. Constructing
    /// one requires a Docker daemon, so this only asserts the type is
    /// nameable — the compile is the signal.
    #[test]
    fn docker_executor_type_is_reachable() {
        fn nameable<T>() {}
        nameable::<adk_rust::code::DockerExecutor>();
    }
}

#[cfg(feature = "codeact")]
mod codeact {
    /// `codeact` forwards `adk-agent/codeact`, exposing the module through
    /// the `agent` glob re-export.
    #[test]
    fn codeact_module_is_reachable() {
        assert!(!adk_rust::agent::codeact::CODEACT_SYSTEM_PROMPT.is_empty());
    }
}

#[cfg(feature = "codeact-monty")]
mod codeact_monty {
    use adk_rust::agent::codeact::CodeRuntime;
    use adk_rust::codeact_monty::MontyRuntime;

    /// `codeact-monty` re-exports the runtime crate and implies `codeact`,
    /// so the runtime constructs and reports its capabilities through the
    /// `CodeRuntime` seam.
    #[test]
    fn monty_runtime_is_reachable() {
        let runtime = MontyRuntime::new();
        assert!(runtime.capabilities().supports_suspension);
    }
}

/// Runs an `LlmAgent` whose model calls one tool, so a test can tell whether the tool ran.
#[cfg(all(
    feature = "agents",
    feature = "runner",
    feature = "sessions",
    any(feature = "guardrail", feature = "plugin")
))]
mod tool_call_harness {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use adk_rust::agent::LlmAgentBuilder;
    use adk_rust::futures::{StreamExt, stream};
    use adk_rust::runner::Runner;
    use adk_rust::session::{CreateRequest, InMemorySessionService, SessionService};
    use adk_rust::{
        Agent, Content, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result, Tool,
        ToolContext, async_trait,
    };
    use serde_json::{Value, json};

    pub const TOOL: &str = "delete_everything";

    /// Calls [`TOOL`] on the first turn and answers with text afterwards.
    struct CallOnce {
        turns: AtomicUsize,
    }

    #[async_trait]
    impl Llm for CallOnce {
        fn name(&self) -> &str {
            "call-once"
        }
        async fn generate_content(
            &self,
            _req: LlmRequest,
            _stream: bool,
        ) -> Result<LlmResponseStream> {
            let content = if self.turns.fetch_add(1, Ordering::SeqCst) == 0 {
                Content {
                    role: "model".to_string(),
                    parts: vec![Part::FunctionCall {
                        name: TOOL.to_string(),
                        args: json!({ "path": "/" }),
                        id: Some("call_1".to_string()),
                        thought_signature: None,
                    }],
                }
            } else {
                Content::new("model").with_text("done")
            };
            Ok(Box::pin(stream::iter([Ok(LlmResponse::new(content))])))
        }
    }

    struct CountingTool {
        runs: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for CountingTool {
        fn name(&self) -> &str {
            TOOL
        }
        fn description(&self) -> &str {
            "counts its executions"
        }
        async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "status": "ok" }))
        }
    }

    /// Runs one turn of the agent `configure` builds and returns how often the tool ran
    /// and every event serialised as JSON.
    pub async fn run(
        configure: impl FnOnce(LlmAgentBuilder) -> LlmAgentBuilder,
    ) -> (usize, String) {
        let runs = Arc::new(AtomicUsize::new(0));
        let builder = LlmAgentBuilder::new("wiring")
            .model(Arc::new(CallOnce { turns: AtomicUsize::new(0) }))
            .tool(Arc::new(CountingTool { runs: Arc::clone(&runs) }));
        let agent: Arc<dyn Agent> = Arc::new(configure(builder).build().expect("agent builds"));

        let sessions = Arc::new(InMemorySessionService::new());
        sessions
            .create(CreateRequest {
                app_name: "wiring".to_string(),
                user_id: "user".to_string(),
                session_id: Some("session".to_string()),
                state: HashMap::new(),
            })
            .await
            .expect("session created");
        let runner = Runner::builder()
            .app_name("wiring")
            .agent(agent)
            .session_service(sessions as Arc<dyn SessionService>)
            .build()
            .expect("runner builds");

        let mut events = runner
            .run_str("user", "session", Content::new("user").with_text("go"))
            .await
            .expect("run starts");
        let mut transcript = String::new();
        while let Some(event) = events.next().await {
            transcript.push_str(&serde_json::to_string(&event.expect("event")).unwrap_or_default());
        }
        (runs.load(Ordering::SeqCst), transcript)
    }
}

#[cfg(all(feature = "agents", feature = "runner", feature = "sessions", feature = "guardrail"))]
mod guardrail {
    use adk_rust::async_trait;
    use adk_rust::guardrail::{
        GuardrailSet, Severity, ToolGuardrail, ToolGuardrailResult, ToolGuardrailSet,
    };
    use serde_json::Value;

    use super::tool_call_harness::{TOOL, run};

    struct DenyEverything;

    #[async_trait]
    impl ToolGuardrail for DenyEverything {
        fn name(&self) -> &str {
            "deny-everything"
        }
        async fn validate_call(&self, _tool: &str, _args: &Value) -> ToolGuardrailResult {
            ToolGuardrailResult::deny("refused by the wiring test", Severity::Critical)
        }
    }

    /// `guardrail` forwards `adk-agent/guardrails`. Without it `LlmAgentBuilder` takes
    /// placeholder guardrail sets that never run, and these `adk_guardrail` sets do not
    /// type-check against it.
    #[tokio::test]
    async fn llm_agent_enforces_tool_guardrails() {
        let (runs, transcript) = run(|builder| {
            builder
                .input_guardrails(GuardrailSet::new())
                .output_guardrails(GuardrailSet::new())
                .tool_guardrails(ToolGuardrailSet::new().with(DenyEverything))
        })
        .await;

        assert_eq!(runs, 0, "a denied {TOOL} call must not execute");
        assert!(transcript.contains("deny-everything"), "got {transcript}");
    }
}

#[cfg(all(feature = "agents", feature = "runner", feature = "sessions", feature = "plugin"))]
mod plugin {
    use std::sync::Arc;

    use adk_rust::plugin::{BeforeToolCallResult, EnhancedPlugin, PluginContext};
    use adk_rust::{CallbackContext, Result, Tool, async_trait};
    use serde_json::{Value, json};

    use super::tool_call_harness::{TOOL, run};

    struct ShortCircuit;

    #[async_trait]
    impl EnhancedPlugin for ShortCircuit {
        fn name(&self) -> &str {
            "short-circuit"
        }
        async fn before_tool_call(
            &self,
            _tool: Arc<dyn Tool>,
            _args: Value,
            _ctx: Arc<dyn CallbackContext>,
            _plugin_ctx: &PluginContext,
        ) -> Result<BeforeToolCallResult> {
            Ok(BeforeToolCallResult::ShortCircuit(json!({ "intercepted_by": "short-circuit" })))
        }
    }

    /// `plugin` forwards `adk-agent/enhanced-plugins`, which adds
    /// `LlmAgentBuilder::enhanced_plugin` and runs the plugin's tool hooks.
    #[tokio::test]
    async fn llm_agent_runs_enhanced_plugins() {
        let (runs, transcript) =
            run(|builder| builder.enhanced_plugin(Arc::new(ShortCircuit))).await;

        assert_eq!(runs, 0, "a short-circuited {TOOL} call must not execute");
        assert!(transcript.contains("intercepted_by"), "got {transcript}");
    }
}

#[cfg(all(feature = "agents", feature = "skills"))]
mod skills {
    use adk_rust::agent::LlmAgentBuilder;
    use adk_rust::skill::{SelectionPolicy, SkillIndex};

    /// `skills` forwards `adk-agent/skills`, which adds the skill methods on
    /// `LlmAgentBuilder`; without it this does not compile.
    #[test]
    fn llm_agent_builder_accepts_skills() {
        let builder = LlmAgentBuilder::new("skilled")
            .with_skills(SkillIndex::default())
            .with_skill_policy(SelectionPolicy::default())
            .with_skill_budget(1_000);
        drop(builder);
    }
}
