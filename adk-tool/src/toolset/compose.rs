use adk_core::{ReadonlyContext, Result, Tool, ToolContext, ToolPredicate, Toolset};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// A toolset wrapper that filters tools from an inner toolset using a predicate.
///
/// Works with any `Toolset` implementation. Tools that do not satisfy the
/// predicate are excluded from the resolved tool list.
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::toolset::{FilteredToolset, string_predicate};
///
/// let browser = BrowserToolset::new(session);
/// let filtered = FilteredToolset::new(
///     Arc::new(browser),
///     string_predicate(vec!["navigate".into(), "click".into()]),
/// );
/// // Only "navigate" and "click" tools will be exposed
/// ```
pub struct FilteredToolset {
    inner: Arc<dyn Toolset>,
    predicate: ToolPredicate,
    name: String,
}

impl FilteredToolset {
    /// Wrap `inner` and keep only tools that satisfy `predicate`.
    pub fn new(inner: Arc<dyn Toolset>, predicate: ToolPredicate) -> Self {
        let name = format!("{}_filtered", inner.name());
        Self { inner, predicate, name }
    }

    /// Wrap `inner` with a custom name and keep only tools that satisfy `predicate`.
    pub fn with_name(
        inner: Arc<dyn Toolset>,
        predicate: ToolPredicate,
        name: impl Into<String>,
    ) -> Self {
        Self { inner, predicate, name: name.into() }
    }
}

#[async_trait]
impl Toolset for FilteredToolset {
    fn name(&self) -> &str {
        &self.name
    }

    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let all = self.inner.tools(ctx).await?;
        Ok(all.into_iter().filter(|t| (self.predicate)(t.as_ref())).collect())
    }
}

/// A toolset that merges tools from multiple inner toolsets into one.
///
/// Toolsets are resolved in registration order. If two inner toolsets
/// provide a tool with the same name, the first one wins (last-registered
/// duplicates are dropped with a `tracing::warn`).
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::toolset::MergedToolset;
///
/// let merged = MergedToolset::new("all_tools", vec![
///     Arc::new(browser_toolset),
///     Arc::new(search_toolset),
/// ]);
/// ```
pub struct MergedToolset {
    name: String,
    inner: Vec<Arc<dyn Toolset>>,
}

impl MergedToolset {
    /// Create a merged toolset from multiple inner toolsets.
    pub fn new(name: impl Into<String>, toolsets: Vec<Arc<dyn Toolset>>) -> Self {
        Self { name: name.into(), inner: toolsets }
    }

    /// Append another toolset to the merge list.
    pub fn with_toolset(mut self, toolset: Arc<dyn Toolset>) -> Self {
        self.inner.push(toolset);
        self
    }
}

#[async_trait]
impl Toolset for MergedToolset {
    fn name(&self) -> &str {
        &self.name
    }

    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let mut seen = std::collections::HashSet::new();
        let mut merged = Vec::new();

        for toolset in &self.inner {
            let tools = toolset.tools(ctx.clone()).await?;
            for tool in tools {
                let tool_name = tool.name().to_string();
                if seen.contains(&tool_name) {
                    tracing::warn!(
                        tool.name = %tool_name,
                        toolset.name = %toolset.name(),
                        merged_toolset.name = %self.name,
                        "duplicate tool name in MergedToolset, skipping"
                    );
                    continue;
                }
                seen.insert(tool_name);
                merged.push(tool);
            }
        }

        Ok(merged)
    }
}

/// A toolset wrapper that prefixes all tool names from an inner toolset.
///
/// Useful for namespacing tools when composing multiple toolsets that
/// might have overlapping tool names.
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::toolset::PrefixedToolset;
///
/// let browser = BrowserToolset::new(session);
/// let prefixed = PrefixedToolset::new(Arc::new(browser), "browser");
/// // Tools become "browser_navigate", "browser_click", etc.
/// ```
pub struct PrefixedToolset {
    inner: Arc<dyn Toolset>,
    prefix: String,
    name: String,
}

impl PrefixedToolset {
    /// Wrap `inner` and prefix all tool names with `prefix_`.
    pub fn new(inner: Arc<dyn Toolset>, prefix: impl Into<String>) -> Self {
        let prefix = prefix.into();
        let name = format!("{}_{}", prefix, inner.name());
        Self { inner, prefix, name }
    }
}

#[async_trait]
impl Toolset for PrefixedToolset {
    fn name(&self) -> &str {
        &self.name
    }

    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let tools = self.inner.tools(ctx).await?;
        Ok(tools
            .into_iter()
            .map(|t| -> Arc<dyn Tool> { Arc::new(PrefixedTool::new(t, &self.prefix)) })
            .collect())
    }
}

/// Internal wrapper that presents a tool under a prefixed name.
struct PrefixedTool {
    inner: Arc<dyn Tool>,
    prefixed_name: String,
    prefixed_description: String,
}

impl PrefixedTool {
    fn new(inner: Arc<dyn Tool>, prefix: &str) -> Self {
        let prefixed_name = format!("{prefix}_{}", inner.name());
        let prefixed_description = inner.description().to_string();
        Self { inner, prefixed_name, prefixed_description }
    }
}

#[async_trait]
impl Tool for PrefixedTool {
    fn name(&self) -> &str {
        &self.prefixed_name
    }

    fn description(&self) -> &str {
        &self.prefixed_description
    }

    fn enhanced_description(&self) -> String {
        self.inner.enhanced_description()
    }

    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }

    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }

    fn required_scopes(&self) -> &[&str] {
        self.inner.required_scopes()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        self.inner.execute(ctx, args).await
    }
}
