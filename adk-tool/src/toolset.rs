use adk_core::{ReadonlyContext, Result, Tool, ToolPredicate, Toolset};
use async_trait::async_trait;
use std::sync::Arc;

pub struct BasicToolset {
    name: String,
    tools: Vec<Arc<dyn Tool>>,
    predicate: Option<ToolPredicate>,
}

impl BasicToolset {
    pub fn new(name: impl Into<String>, tools: Vec<Arc<dyn Tool>>) -> Self {
        Self { name: name.into(), tools, predicate: None }
    }

    pub fn with_predicate(mut self, predicate: ToolPredicate) -> Self {
        self.predicate = Some(predicate);
        self
    }
}

#[async_trait]
impl Toolset for BasicToolset {
    fn name(&self) -> &str {
        &self.name
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        if let Some(predicate) = &self.predicate {
            Ok(self.tools.iter().filter(|tool| predicate(tool.as_ref())).cloned().collect())
        } else {
            Ok(self.tools.clone())
        }
    }
}

/// Creates a predicate that allows only tools with names in the provided list
pub fn string_predicate(allowed_tools: Vec<String>) -> ToolPredicate {
    let allowed_set: std::collections::HashSet<String> = allowed_tools.into_iter().collect();
    Box::new(move |tool: &dyn Tool| allowed_set.contains(tool.name()))
}
