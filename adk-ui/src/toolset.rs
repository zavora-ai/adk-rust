use crate::tools::*;
use adk_core::{ReadonlyContext, Result, Tool, Toolset};
use async_trait::async_trait;
use std::sync::Arc;

/// A toolset containing all UI rendering tools.
///
/// Use this to easily add UI capabilities to an agent:
///
/// ```rust,ignore
/// use adk_ui::UiToolset;
/// use adk_agent::LlmAgentBuilder;
///
/// let tools = UiToolset::all_tools();
/// let mut builder = LlmAgentBuilder::new("assistant");
/// for tool in tools {
///     builder = builder.tool(tool);
/// }
/// let agent = builder.build()?;
/// ```
pub struct UiToolset {
    include_form: bool,
    include_card: bool,
    include_alert: bool,
    include_confirm: bool,
    include_table: bool,
    include_chart: bool,
    include_layout: bool,
    include_progress: bool,
    include_modal: bool,
    include_toast: bool,
}

impl UiToolset {
    /// Create a new UiToolset with all tools enabled
    pub fn new() -> Self {
        Self {
            include_form: true,
            include_card: true,
            include_alert: true,
            include_confirm: true,
            include_table: true,
            include_chart: true,
            include_layout: true,
            include_progress: true,
            include_modal: true,
            include_toast: true,
        }
    }

    /// Create a toolset with only form rendering
    pub fn forms_only() -> Self {
        Self {
            include_form: true,
            include_card: false,
            include_alert: false,
            include_confirm: false,
            include_table: false,
            include_chart: false,
            include_layout: false,
            include_progress: false,
            include_modal: false,
            include_toast: false,
        }
    }

    /// Disable form rendering
    pub fn without_form(mut self) -> Self {
        self.include_form = false;
        self
    }

    /// Disable card rendering
    pub fn without_card(mut self) -> Self {
        self.include_card = false;
        self
    }

    /// Disable alert rendering
    pub fn without_alert(mut self) -> Self {
        self.include_alert = false;
        self
    }

    /// Disable confirm rendering
    pub fn without_confirm(mut self) -> Self {
        self.include_confirm = false;
        self
    }

    /// Disable table rendering
    pub fn without_table(mut self) -> Self {
        self.include_table = false;
        self
    }

    /// Disable chart rendering
    pub fn without_chart(mut self) -> Self {
        self.include_chart = false;
        self
    }

    /// Disable layout rendering
    pub fn without_layout(mut self) -> Self {
        self.include_layout = false;
        self
    }

    /// Disable progress rendering
    pub fn without_progress(mut self) -> Self {
        self.include_progress = false;
        self
    }

    /// Disable modal rendering
    pub fn without_modal(mut self) -> Self {
        self.include_modal = false;
        self
    }

    /// Disable toast rendering
    pub fn without_toast(mut self) -> Self {
        self.include_toast = false;
        self
    }

    /// Get all tools as a Vec for use with LlmAgentBuilder
    pub fn all_tools() -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(RenderFormTool::new()) as Arc<dyn Tool>,
            Arc::new(RenderCardTool::new()),
            Arc::new(RenderAlertTool::new()),
            Arc::new(RenderConfirmTool::new()),
            Arc::new(RenderTableTool::new()),
            Arc::new(RenderChartTool::new()),
            Arc::new(RenderLayoutTool::new()),
            Arc::new(RenderProgressTool::new()),
            Arc::new(RenderModalTool::new()),
            Arc::new(RenderToastTool::new()),
        ]
    }
}

impl Default for UiToolset {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Toolset for UiToolset {
    fn name(&self) -> &str {
        "ui"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

        if self.include_form {
            tools.push(Arc::new(RenderFormTool::new()));
        }
        if self.include_card {
            tools.push(Arc::new(RenderCardTool::new()));
        }
        if self.include_alert {
            tools.push(Arc::new(RenderAlertTool::new()));
        }
        if self.include_confirm {
            tools.push(Arc::new(RenderConfirmTool::new()));
        }
        if self.include_table {
            tools.push(Arc::new(RenderTableTool::new()));
        }
        if self.include_chart {
            tools.push(Arc::new(RenderChartTool::new()));
        }
        if self.include_layout {
            tools.push(Arc::new(RenderLayoutTool::new()));
        }
        if self.include_progress {
            tools.push(Arc::new(RenderProgressTool::new()));
        }
        if self.include_modal {
            tools.push(Arc::new(RenderModalTool::new()));
        }
        if self.include_toast {
            tools.push(Arc::new(RenderToastTool::new()));
        }

        Ok(tools)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_tools_returns_10_tools() {
        let tools = UiToolset::all_tools();
        assert_eq!(tools.len(), 10);

        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"render_form"));
        assert!(names.contains(&"render_card"));
        assert!(names.contains(&"render_alert"));
        assert!(names.contains(&"render_confirm"));
        assert!(names.contains(&"render_table"));
        assert!(names.contains(&"render_chart"));
        assert!(names.contains(&"render_layout"));
        assert!(names.contains(&"render_progress"));
        assert!(names.contains(&"render_modal"));
        assert!(names.contains(&"render_toast"));
    }

    #[test]
    fn test_forms_only() {
        let toolset = UiToolset::forms_only();
        assert!(toolset.include_form);
        assert!(!toolset.include_card);
        assert!(!toolset.include_alert);
        assert!(!toolset.include_table);
    }

    #[test]
    fn test_without_methods() {
        let toolset = UiToolset::new().without_chart().without_table().without_progress();

        assert!(toolset.include_form);
        assert!(toolset.include_card);
        assert!(!toolset.include_chart);
        assert!(!toolset.include_table);
        assert!(!toolset.include_progress);
    }

    #[test]
    fn test_toolset_name() {
        let toolset = UiToolset::new();
        assert_eq!(toolset.name(), "ui");
    }

    #[test]
    fn test_default_is_new() {
        let default = UiToolset::default();
        let new = UiToolset::new();
        assert_eq!(default.include_form, new.include_form);
        assert_eq!(default.include_card, new.include_card);
        assert_eq!(default.include_chart, new.include_chart);
    }
}
