//! `HarnessToolset`: the harness tools and per-request reminders (spec 3.1 §7.6, §7.7).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_core::{Content, LlmRequest, ReadonlyContext, Result, Tool, Toolset};
use async_trait::async_trait;

use crate::mode::{HarnessMode, ModeStore};
use crate::tools::TodoList;

/// Harness work that runs before each model request, ahead of the reminders: identity
/// observation (spec 3.1 §9.2 I6) and the loop guard (§7.13).
#[async_trait]
pub(crate) trait RequestObserver: Send + Sync {
    /// Observes `ctx` before its next model request; an error ends the turn.
    async fn before_request(
        &self,
        ctx: &Arc<dyn ReadonlyContext>,
        request: &mut LlmRequest,
    ) -> Result<()>;
}

/// The harness tools as one [`Toolset`] (spec 3.1 §7.1).
///
/// The tool list is fixed for the agent's lifetime — `exit_plan_mode` is declared in
/// every mode — so the tool surface and the provider's prompt cache stay stable; the
/// policy decides what each mode allows. [`process_llm_request`](Toolset::process_llm_request)
/// appends a one-line mode banner to every request and a reminder of the open todos when
/// the list changed, at the end of the request, keeping the cached prefix stable
/// (spec 3.1 §7.6, §7.7).
///
/// # Example
///
/// ```rust,ignore
/// let names = harness.runtime().toolset().tool_names();
/// assert!(names.contains(&"exit_plan_mode".to_string()));
/// ```
pub struct HarnessToolset {
    tools: Vec<Arc<dyn Tool>>,
    mode_store: Arc<dyn ModeStore>,
    default_mode: HarnessMode,
    reminded: Mutex<HashMap<String, u64>>,
    observers: Vec<Arc<dyn RequestObserver>>,
    #[cfg(feature = "memory")]
    facts: Option<Arc<crate::facts::runtime::FactsRuntime>>,
}

impl HarnessToolset {
    pub(crate) fn new(
        tools: Vec<Arc<dyn Tool>>,
        mode_store: Arc<dyn ModeStore>,
        default_mode: HarnessMode,
    ) -> Self {
        Self {
            tools,
            mode_store,
            default_mode,
            reminded: Mutex::new(HashMap::new()),
            observers: Vec::new(),
            #[cfg(feature = "memory")]
            facts: None,
        }
    }

    /// Recalls long-term facts into each user turn (spec 3.1 §7.15.2).
    #[cfg(feature = "memory")]
    pub(crate) fn with_facts(mut self, facts: Arc<crate::facts::runtime::FactsRuntime>) -> Self {
        self.facts = Some(facts);
        self
    }

    /// The harness's long-term memory, when on.
    #[cfg(feature = "memory")]
    pub(crate) fn facts(&self) -> Option<&Arc<crate::facts::runtime::FactsRuntime>> {
        self.facts.as_ref()
    }

    /// Adds work to run before each model request, in order; a team member gets its
    /// coordinator's (spec 3.1 §7.10).
    pub(crate) fn with_observers(mut self, observers: &[Arc<dyn RequestObserver>]) -> Self {
        self.observers.extend(observers.iter().cloned());
        self
    }

    /// The toolset of a team member (spec 3.1 §7.10): `tools` without `exit_plan_mode`,
    /// `memory_write`, `memory_delete`, `memory_search`, `memory_remember` or any agent
    /// delegation, so a member can neither change the mode nor delegate further, and
    /// neither recalls nor writes long-term facts (§9.2 I42). The member reads the parent's mode, because it
    /// shares the parent's (app, user, session).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::{HarnessMode, HarnessToolset, InMemoryModeStore};
    /// use std::sync::Arc;
    ///
    /// let plan_exit = Arc::new(adk_harness::ExitPlanModeTool::new(
    ///     Arc::new(InMemoryModeStore::new()),
    ///     HarnessMode::Ask,
    /// ));
    /// let toolset = HarnessToolset::member(
    ///     vec![plan_exit],
    ///     Arc::new(InMemoryModeStore::new()),
    ///     HarnessMode::Ask,
    /// );
    /// assert!(toolset.tool_names().is_empty());
    /// ```
    pub fn member(
        tools: Vec<Arc<dyn Tool>>,
        mode_store: Arc<dyn ModeStore>,
        default_mode: HarnessMode,
    ) -> Self {
        let tools = tools
            .into_iter()
            .filter(|tool| {
                !matches!(
                    tool.name(),
                    "exit_plan_mode"
                        | "memory_write"
                        | "memory_delete"
                        | "memory_search"
                        | "memory_remember"
                ) && !tool.is_agent_delegation()
            })
            .collect();
        Self::new(tools, mode_store, default_mode)
    }

    /// Names of the tools, in declaration order.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.iter().map(|tool| tool.name().to_string()).collect()
    }
}

/// The one-line banner describing what `mode` allows.
pub(crate) fn mode_banner(mode: HarnessMode) -> String {
    let rule = match mode {
        HarnessMode::ReadOnly => "read-only: you may read and keep todos; nothing else runs",
        HarnessMode::Plan => {
            "plan: investigate with read-only tools, then present a plan with exit_plan_mode; \
             edits and commands are refused until the user approves the plan"
        }
        HarnessMode::Ask => {
            "ask: call the edit and command tools you need directly; the harness asks the user \
             to approve each call before it runs, so do not ask for permission in your reply \
             or wait for a go-ahead, and do not call exit_plan_mode"
        }
        HarnessMode::Auto => {
            "auto: call tools directly; edits run without asking and the harness asks the \
             user before host commands"
        }
    };
    format!("Harness mode is {rule}.")
}

fn todo_reminder(list: &TodoList) -> String {
    let open: Vec<String> = list
        .open_items()
        .map(|item| {
            format!(
                "- [{}] {}: {}",
                serde_json::to_value(item.status)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default(),
                item.id,
                item.content
            )
        })
        .collect();
    if open.is_empty() {
        "All todos are complete.".to_string()
    } else {
        format!("Open todos (revision {}):\n{}", list.revision, open.join("\n"))
    }
}

#[async_trait]
impl Toolset for HarnessToolset {
    fn name(&self) -> &str {
        "harness"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        Ok(self.tools.clone())
    }

    async fn process_llm_request(
        &self,
        ctx: Arc<dyn ReadonlyContext>,
        request: &mut LlmRequest,
    ) -> Result<()> {
        for observer in &self.observers {
            observer.before_request(&ctx, request).await?;
        }
        #[cfg(feature = "memory")]
        if let Some(facts) = &self.facts {
            facts.before_request(&ctx, request).await?;
        }
        let mode = match self.mode_store.get(ctx.app_name(), ctx.user_id(), ctx.session_id()).await
        {
            Ok(Some(record)) => record.mode_for_invocation(ctx.invocation_id()),
            Ok(None) => self.default_mode,
            Err(_) => HarnessMode::ReadOnly,
        };
        let mut lines = vec![mode_banner(mode)];
        if let Some(list) = ctx.state().map(TodoList::from_state)
            && list.revision > 0
        {
            let key = format!("{}\u{1f}{}", ctx.session_id(), ctx.agent_name());
            let mut reminded = self.reminded.lock().unwrap_or_else(|e| e.into_inner());
            if reminded.len() > 4096 {
                reminded.clear();
            }
            if reminded.get(&key) != Some(&list.revision) {
                reminded.insert(key, list.revision);
                lines.push(todo_reminder(&list));
            }
        }
        request.contents.push(
            Content::new("user")
                .with_text(format!("<system-reminder>\n{}\n</system-reminder>", lines.join("\n"))),
        );
        Ok(())
    }
}
