//! `HookedTool`: a tool whose executions run the `PostTool` and `PostEdit` hooks
//! (spec 3.1 §7.22).
//!
//! The wrapper runs after `execute` returns, so no hook runs between
//! `authorize_tool_call` and `execute` (spec 3.1 §9.2 I53). It forwards every declaration
//! method, so the tool's `ToolOrigin` — and every standing rule and held call bound to
//! it — is unchanged.

use std::sync::Arc;
use std::time::Instant;

use adk_core::{EventActions, Result, Tool, ToolContext, ToolEffect, ToolOrigin};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::PostToolVerdict;
use super::runner::{HookCall, HookEdit, HookResult, HookRunner};
use crate::audit::HarnessAudit;
use crate::mode::{HarnessMode, ModeStore};
use crate::policy::{Isolation, ToolClassRegistry, ToolKind};

/// Files larger than this get no line diff in the `PostEdit` input, only their digests.
const DIFF_INPUT_MAX_BYTES: usize = 256 * 1024;
/// The longest diff the `PostEdit` input carries.
const DIFF_MAX_BYTES: usize = 32 * 1024;

/// A tool whose executions run the `PostTool` hooks, and the `PostEdit` hooks when it is
/// `write_file`, `edit_file` or `apply_patch` — once per file a patch changed (spec 3.1
/// §7.22, §7.26).
pub(crate) struct HookedTool {
    inner: Arc<dyn Tool>,
    hooks: Arc<HookRunner>,
    class: String,
    modes: (Arc<dyn ModeStore>, HarnessMode),
    edit: bool,
}

impl HookedTool {
    /// Wraps every tool in `tools`, classifying each with `registry` as the policy would.
    pub(crate) fn wrap_all(
        tools: Vec<Arc<dyn Tool>>,
        hooks: &Arc<HookRunner>,
        registry: &ToolClassRegistry,
        agent: &str,
        isolation: Isolation,
        modes: (Arc<dyn ModeStore>, HarnessMode),
    ) -> Vec<Arc<dyn Tool>> {
        tools
            .into_iter()
            .map(|tool| {
                let origin = ToolOrigin::of(tool.as_ref());
                let kind = registry.classify(
                    agent,
                    tool.name(),
                    Some(&origin),
                    &json!({}),
                    tool.effect(),
                    isolation,
                    false,
                );
                let edit = matches!(tool.name(), "write_file" | "edit_file" | "apply_patch");
                Arc::new(Self {
                    inner: tool,
                    hooks: hooks.clone(),
                    class: class_label(&kind),
                    modes: modes.clone(),
                    edit,
                }) as Arc<dyn Tool>
            })
            .collect()
    }

    async fn mode(&self, ctx: &dyn ToolContext) -> HarnessMode {
        let (store, default_mode) = &self.modes;
        match store.get(ctx.app_name(), ctx.user_id(), ctx.session_id()).await {
            Ok(Some(record)) => record.mode_for_invocation(ctx.invocation_id()),
            Ok(None) => *default_mode,
            Err(error) => {
                tracing::warn!(error = %error, "mode store unavailable; hooks run as read_only");
                HarnessMode::ReadOnly
            }
        }
    }

    /// The files an edit targets, as the workspace resolves them: every path of a patch.
    fn edit_targets(&self, args: &Value) -> Vec<std::path::PathBuf> {
        if !self.edit {
            return Vec::new();
        }
        crate::edits::edit_paths(self.inner.name(), args)
            .unwrap_or_default()
            .iter()
            .filter_map(|path| self.hooks.workspace().resolve(path).ok())
            .collect()
    }
}

/// The class label hooks see: `read`, `edit`, `shell_host`, `external`, ….
pub(crate) fn class_label(kind: &ToolKind) -> String {
    match kind {
        ToolKind::Read => "read".to_string(),
        ToolKind::Todo => "todo".to_string(),
        ToolKind::PlanExit => "plan_exit".to_string(),
        ToolKind::Edit => "edit".to_string(),
        ToolKind::Shell(Isolation::Host) => "shell_host".to_string(),
        ToolKind::Shell(Isolation::OsSandbox) => "shell_os_sandbox".to_string(),
        ToolKind::Shell(Isolation::Container) => "shell_container".to_string(),
        ToolKind::ShellEscalation => "shell_escalation".to_string(),
        ToolKind::Delegate => "delegate".to_string(),
        ToolKind::MemoryWrite => "memory_write".to_string(),
        ToolKind::MemoryPropose => "memory_propose".to_string(),
        ToolKind::Payment => "payment".to_string(),
        ToolKind::ComputerUse => "computer_use".to_string(),
        ToolKind::Web { allowlisted } => {
            format!("web_{}", if *allowlisted { "allowlisted" } else { "other" })
        }
        ToolKind::External { effect, trusted_origin } => {
            let effect = match effect {
                ToolEffect::ReadOnly => "read_only",
                ToolEffect::Idempotent => "idempotent",
                ToolEffect::NonIdempotent => "non_idempotent",
                _ => "other",
            };
            let trust = if *trusted_origin { "trusted" } else { "untrusted" };
            format!("external_{effect}_{trust}")
        }
    }
}

/// A capped line diff of `before` and `after`: the common prefix and suffix are dropped,
/// the changed middle is shown as `-` and `+` lines under one hunk header.
pub(crate) fn line_diff(before: &str, after: &str) -> String {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let removed = &old[prefix..old.len() - suffix];
    let added = &new[prefix..new.len() - suffix];
    if removed.is_empty() && added.is_empty() {
        return String::new();
    }
    let mut out =
        format!("@@ -{},{} +{},{} @@\n", prefix + 1, removed.len(), prefix + 1, added.len());
    for line in removed {
        out.push('-');
        out.push_str(line);
        out.push('\n');
        if out.len() > DIFF_MAX_BYTES {
            break;
        }
    }
    for line in added {
        if out.len() > DIFF_MAX_BYTES {
            break;
        }
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    if out.len() > DIFF_MAX_BYTES {
        out.truncate(out.floor_char_boundary(DIFF_MAX_BYTES));
        out.push_str("\n…[truncated]\n");
    }
    out
}

#[async_trait]
impl Tool for HookedTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn declaration(&self) -> Value {
        self.inner.declaration()
    }

    fn enhanced_description(&self) -> String {
        self.inner.enhanced_description()
    }

    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }

    fn is_builtin(&self) -> bool {
        self.inner.is_builtin()
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }

    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }

    fn annotations(&self) -> Option<&Value> {
        self.inner.annotations()
    }

    fn origin(&self) -> &str {
        self.inner.origin()
    }

    fn required_scopes(&self) -> &[&str] {
        self.inner.required_scopes()
    }

    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }

    fn effect(&self) -> ToolEffect {
        self.inner.effect()
    }

    fn timeout_override(&self) -> Option<Option<std::time::Duration>> {
        self.inner.timeout_override()
    }

    fn is_concurrency_safe(&self) -> bool {
        self.inner.is_concurrency_safe()
    }

    fn is_agent_delegation(&self) -> bool {
        self.inner.is_agent_delegation()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let targets = self.edit_targets(&args);
        let mut befores = Vec::with_capacity(targets.len());
        for path in &targets {
            befores.push(tokio::fs::read(path).await.ok());
        }
        let started = Instant::now();
        let result = self.inner.execute(ctx.clone(), args.clone()).await;
        let duration = started.elapsed();
        let edited = !targets.is_empty() && result.is_ok();

        let mode = self.mode(ctx.as_ref()).await;
        let audit = HarnessAudit::for_tool(ctx.as_ref());
        let acting = ctx.acting_context();
        let call_id = ctx.function_call_id().to_string();
        let call = HookCall {
            tool: self.inner.name(),
            args: &args,
            class: &self.class,
            acting: Some(&acting),
            function_call_id: Some(&call_id),
        };
        let (result_value, is_error) = match &result {
            Ok(value) => (value.clone(), false),
            Err(error) => (json!({ "error": error.message }), true),
        };
        let exit_code = result_value.get("exit_code").and_then(Value::as_i64);
        let verdict = self
            .hooks
            .post_tool(
                &call,
                &HookResult { result: &result_value, is_error, exit_code, duration },
                mode,
                ctx.as_ref(),
                audit.as_ref(),
            )
            .await;
        // A failed tool stays failed: hooks observe the error and cannot turn it into a
        // result the model would act on.
        let mut result = match result {
            Ok(value) => Ok(verdict.apply(value)),
            Err(error) => Err(error),
        };

        if edited {
            for (path, before) in targets.iter().zip(befores) {
                let after = tokio::fs::read(path).await.ok();
                if before == after && before.is_some() {
                    continue;
                }
                if before.is_none() && after.is_none() {
                    continue;
                }
                let before_sha = before.as_deref().map(crate::fsutil::hex_digest);
                let after_sha = after.as_deref().map(crate::fsutil::hex_digest).unwrap_or_default();
                let diff = match (&before, &after) {
                    (_, Some(after_bytes))
                        if before.as_ref().is_none_or(|b| b.len() <= DIFF_INPUT_MAX_BYTES)
                            && after_bytes.len() <= DIFF_INPUT_MAX_BYTES =>
                    {
                        let before_text =
                            before.as_deref().map(String::from_utf8_lossy).unwrap_or_default();
                        line_diff(&before_text, &String::from_utf8_lossy(after_bytes))
                    }
                    _ => String::new(),
                };
                let relative = self.hooks.workspace().display(path);
                let verdict = self
                    .hooks
                    .post_edit(
                        &HookEdit {
                            tool: self.inner.name(),
                            path: &relative,
                            before_sha256: before_sha.as_deref(),
                            after_sha256: &after_sha,
                            diff: &diff,
                        },
                        mode,
                        ctx.as_ref(),
                        audit.as_ref(),
                    )
                    .await;
                if let Ok(value) = result {
                    result = Ok(match verdict {
                        PostToolVerdict::Annotate { .. } if value.get("error").is_some() => value,
                        verdict => verdict.apply(value),
                    });
                }
                // A hook that rewrote the file — a formatter — is part of the edit: the
                // agent's read record follows it, so its next edit is not stale (§7.26).
                if after.is_some() && tokio::fs::read(path).await.ok() != after {
                    let workspace = self.hooks.workspace();
                    let relative = workspace.record_path(path);
                    if let Err(error) = workspace.record_current(ctx.as_ref(), &relative).await {
                        tracing::debug!(error = %error, "read record not refreshed after a hook");
                    }
                }
            }
        }

        // The turn's hook budget is spent: the result carries the reason and the turn ends
        // after this call (spec 3.1 §7.22).
        if self.hooks.over_budget(ctx.invocation_id()) {
            let mut actions: EventActions = ctx.actions();
            actions.skip_summarization = true;
            ctx.set_actions(actions);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diffs_show_only_the_changed_middle() {
        let diff = line_diff("a\nb\nc\nd\n", "a\nB\nc\nd\n");
        assert_eq!(diff, "@@ -2,1 +2,1 @@\n-b\n+B\n");
        assert_eq!(line_diff("same\n", "same\n"), "");
        let added = line_diff("", "x\ny\n");
        assert_eq!(added, "@@ -1,0 +1,2 @@\n+x\n+y\n");
        let removed = line_diff("x\ny\n", "y\n");
        assert_eq!(removed, "@@ -1,1 +1,0 @@\n-x\n");
    }

    #[test]
    fn long_diffs_are_cut() {
        let before = String::new();
        let after: String = (0..5000).map(|i| format!("line {i} {}\n", "x".repeat(20))).collect();
        let diff = line_diff(&before, &after);
        assert!(diff.len() <= DIFF_MAX_BYTES + 32);
        assert!(diff.ends_with("…[truncated]\n"));
    }

    #[test]
    fn class_labels_cover_every_kind() {
        assert_eq!(class_label(&ToolKind::Shell(Isolation::Host)), "shell_host");
        assert_eq!(
            class_label(&ToolKind::External {
                effect: ToolEffect::ReadOnly,
                trusted_origin: false
            }),
            "external_read_only_untrusted"
        );
        assert_eq!(class_label(&ToolKind::Edit), "edit");
    }
}
