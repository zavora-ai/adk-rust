//! Provider-executed search and URL tools (spec 3.1 §7.20 "Provider-native search and
//! fetch", §9.2 I49).
//!
//! These run inside the model, outside the governed path. The toolset declares them only
//! to a run that starts in Ask or Auto, for the harness model's provider only, carries the
//! allowlist where the provider supports one, says so in the request, and ends the turn
//! when the mode tightens while one is live.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use adk_core::audit_log::AuditActor;
use adk_core::{
    AdkError, Content, ErrorCategory, ErrorComponent, InvocationContext, LlmRequest,
    ReadonlyContext, Result, Tool, Toolset, async_trait, vendor_for_model,
};
use serde_json::json;

use super::ProviderWebTools;
use crate::audit::HarnessAudit;
use crate::mode::{HarnessMode, ModeStore};

/// Error code of a turn ended because the mode tightened while a provider tool was live.
pub const PROVIDER_WEB_TIGHTENED_CODE: &str = "harness.web.provider_tightened";

struct Entry {
    provider: &'static str,
    tool: Arc<dyn Tool>,
    allowlist: Option<Vec<String>>,
}

/// The provider-executed web tools the operator enabled, declared per run (spec 3.1 §7.20).
pub(crate) struct ProviderWebToolset {
    entries: Vec<Entry>,
    mode_store: Arc<dyn ModeStore>,
    default_mode: HarnessMode,
    /// Invocations the tools were declared to.
    attached: Mutex<HashSet<String>>,
    /// Invocations whose `harness.web.provider_enabled` records were written.
    recorded: Mutex<HashSet<String>>,
}

impl ProviderWebToolset {
    /// Builds the toolset for the model named `model_name`, keeping the tools of its
    /// provider only; a tool enabled for another provider is skipped with a warning.
    pub(crate) fn new(
        tools: ProviderWebTools,
        model_name: &str,
        allowlist: Vec<String>,
        mode_store: Arc<dyn ModeStore>,
        default_mode: HarnessMode,
    ) -> Result<Self> {
        let vendor = vendor_for_model(model_name);
        let domains = (!allowlist.is_empty()).then_some(allowlist);
        let mut entries = Vec::new();
        let mut skipped = Vec::new();
        if tools.anthropic_web_search {
            if vendor == "anthropic" {
                let mut tool = adk_tool::builtin::WebSearchTool::new();
                if let Some(domains) = &domains {
                    tool = tool.with_allowed_domains(domains.clone());
                }
                entries.push(Entry {
                    provider: "anthropic",
                    tool: Arc::new(tool),
                    allowlist: domains.clone(),
                });
            } else {
                skipped.push("anthropic web_search");
            }
        }
        if tools.openai_web_search {
            if vendor == "openai" {
                let mut tool = adk_tool::builtin::OpenAIWebSearchTool::new();
                if let Some(domains) = &domains {
                    tool = tool.with_allowed_domains(domains.clone());
                }
                entries.push(Entry {
                    provider: "openai",
                    tool: Arc::new(tool),
                    allowlist: domains.clone(),
                });
            } else {
                skipped.push("openai web_search");
            }
        }
        if tools.gemini_google_search {
            if vendor == "gemini" {
                entries.push(Entry {
                    provider: "gemini",
                    tool: Arc::new(adk_tool::builtin::GoogleSearchTool::new()),
                    allowlist: None,
                });
            } else {
                skipped.push("gemini google_search");
            }
        }
        if tools.gemini_url_context {
            if vendor == "gemini" {
                entries.push(Entry {
                    provider: "gemini",
                    tool: Arc::new(adk_tool::builtin::UrlContextTool::new()),
                    allowlist: None,
                });
            } else {
                skipped.push("gemini url_context");
            }
        }
        if !skipped.is_empty() {
            tracing::warn!(
                model = model_name,
                vendor,
                skipped = ?skipped,
                "provider web tools enabled for another provider are not attached"
            );
        }
        Ok(Self {
            entries,
            mode_store,
            default_mode,
            attached: Mutex::new(HashSet::new()),
            recorded: Mutex::new(HashSet::new()),
        })
    }

    /// Names of the attached tools, in order.
    pub(crate) fn tool_names(&self) -> Vec<String> {
        self.entries.iter().map(|entry| entry.tool.name().to_string()).collect()
    }

    async fn mode(&self, app: &str, user: &str, session: &str, invocation: &str) -> HarnessMode {
        match self.mode_store.get(app, user, session).await {
            Ok(Some(record)) => record.mode_for_invocation(invocation),
            Ok(None) => self.default_mode,
            Err(_) => HarnessMode::ReadOnly,
        }
    }

    /// The request banner naming which path is governed (spec 3.1 §7.20).
    pub(crate) fn banner(&self) -> String {
        let live: Vec<String> = self
            .entries
            .iter()
            .map(|entry| match &entry.allowlist {
                Some(domains) => {
                    format!(
                        "{} {} (allowlist: {})",
                        entry.provider,
                        entry.tool.name(),
                        domains.join(", ")
                    )
                }
                None => format!("{} {} (every domain)", entry.provider, entry.tool.name()),
            })
            .collect();
        format!(
            "Provider-native web tools are live for this turn: {}. They run inside the model \
             outside the harness's governance; web_fetch is the governed path and its results \
             are screened.",
            live.join("; ")
        )
    }

    /// Records `harness.web.provider_enabled` once per run that starts in Ask or Auto
    /// (spec 3.1 §9.3).
    ///
    /// # Errors
    ///
    /// Returns the audit error under `AuditFailureMode::Block`.
    pub(crate) async fn record_run_start(&self, ctx: &dyn InvocationContext) -> Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }
        let mode =
            self.mode(ctx.app_name(), ctx.user_id(), ctx.session_id(), ctx.invocation_id()).await;
        if !matches!(mode, HarnessMode::Ask | HarnessMode::Auto) {
            return Ok(());
        }
        let first = self
            .recorded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ctx.invocation_id().to_string());
        let Some(trail) = ctx.run_config().audit.clone() else {
            return Ok(());
        };
        if !first {
            return Ok(());
        }
        let audit = HarnessAudit::new(
            trail,
            AuditActor::new(&ctx.acting_context(), ctx.invocation_id().to_string()),
            ctx.session_id(),
        );
        for entry in &self.entries {
            audit
                .record(
                    "harness.web.provider_enabled",
                    json!({
                        "provider": entry.provider,
                        "tool": entry.tool.name(),
                        "allowlist": entry.allowlist,
                        "mode": mode,
                    }),
                )
                .await?;
        }
        Ok(())
    }
}

#[async_trait]
impl Toolset for ProviderWebToolset {
    fn name(&self) -> &str {
        "harness-provider-web"
    }

    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let mode =
            self.mode(ctx.app_name(), ctx.user_id(), ctx.session_id(), ctx.invocation_id()).await;
        let mut attached = self.attached.lock().unwrap_or_else(|e| e.into_inner());
        if attached.len() > 4096 {
            attached.clear();
        }
        // Declared only to a run that starts in Ask or Auto (I49): the first request of
        // the invocation decides, and a later tightening ends the turn below.
        let live = if attached.contains(ctx.invocation_id()) {
            true
        } else if matches!(mode, HarnessMode::Ask | HarnessMode::Auto) {
            attached.insert(ctx.invocation_id().to_string());
            true
        } else {
            false
        };
        Ok(if live {
            self.entries.iter().map(|entry| entry.tool.clone()).collect()
        } else {
            Vec::new()
        })
    }

    async fn process_llm_request(
        &self,
        ctx: Arc<dyn ReadonlyContext>,
        request: &mut LlmRequest,
    ) -> Result<()> {
        let live =
            self.attached.lock().unwrap_or_else(|e| e.into_inner()).contains(ctx.invocation_id());
        if !live {
            return Ok(());
        }
        let mode =
            self.mode(ctx.app_name(), ctx.user_id(), ctx.session_id(), ctx.invocation_id()).await;
        if matches!(mode, HarnessMode::ReadOnly | HarnessMode::Plan) {
            // Ending the turn is the only way to retire a tool the provider already holds.
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Forbidden,
                PROVIDER_WEB_TIGHTENED_CODE,
                format!(
                    "the mode tightened to {mode} while a provider-executed web tool was live; \
                     the turn ends so no ungoverned fetch runs (spec 3.1 §7.20)"
                ),
            ));
        }
        request.contents.push(
            Content::new("user")
                .with_text(format!("<system-reminder>\n{}\n</system-reminder>", self.banner())),
        );
        Ok(())
    }
}
