//! In-process driver tools: a class's tools over an adapter, named and classed as its
//! manifest names and classes them (spec 3.5 §7.13).

use std::sync::Arc;

use adk_core::{Tool, ToolContext, ToolEffect};
use adk_kernel::drivers::EffectClass;
use async_trait::async_trait;
use futures::future::BoxFuture;
use serde_json::Value;

use crate::class::ToolSpec;
use crate::error::Result;
use crate::outbound::IN_PROCESS_ORIGIN_PREFIX;

type Run = dyn Fn(Value) -> BoxFuture<'static, Result<Value>> + Send + Sync;

/// One class tool over an in-process adapter.
pub(crate) struct DriverTool {
    spec: &'static ToolSpec,
    origin: String,
    run: Arc<Run>,
}

impl DriverTool {
    /// The tool of `spec` for driver `driver`, running `run`.
    pub(crate) fn shared(
        spec: &'static ToolSpec,
        driver: &str,
        run: impl Fn(Value) -> BoxFuture<'static, Result<Value>> + Send + Sync + 'static,
    ) -> Arc<dyn Tool> {
        Arc::new(Self {
            spec,
            origin: format!("{IN_PROCESS_ORIGIN_PREFIX}{driver}"),
            run: Arc::new(run),
        })
    }
}

#[async_trait]
impl Tool for DriverTool {
    fn name(&self) -> &str {
        self.spec.name
    }

    fn description(&self) -> &str {
        self.spec.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some((self.spec.schema)())
    }

    fn effect(&self) -> ToolEffect {
        self.spec.class.tool_effect()
    }

    fn origin(&self) -> &str {
        &self.origin
    }

    fn is_read_only(&self) -> bool {
        self.spec.class == EffectClass::Read
    }

    fn is_concurrency_safe(&self) -> bool {
        self.spec.class == EffectClass::Read
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        Ok((self.run)(args).await?)
    }
}

/// The spec of `name` in `tools`; the tables are the crate's own, so a missing name is a
/// bug caught by the crate's tests.
pub(crate) fn spec(tools: &'static [ToolSpec], name: &str) -> &'static ToolSpec {
    tools
        .iter()
        .find(|spec| spec.name == name)
        .unwrap_or_else(|| unreachable!("driver tool `{name}` is in its class table"))
}
