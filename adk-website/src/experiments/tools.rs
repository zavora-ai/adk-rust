//! The Growth agent's experiment tools over the runner (spec 3.5 §7.12, §7.20 "Experiments"):
//! `experiment_start` and `experiment_report`, the implementations of `adk-harness`' catalogue
//! entries, so the ladder decides them as the experiments job's and a standing rule pins them.

use std::sync::Arc;

use adk_core::{Result, Tool, ToolContext, ToolEffect};
use adk_harness::website::{BACK_OFFICE_ORIGIN, EXPERIMENT_REPORT, EXPERIMENT_START};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::store::{ExperimentRecord, ExperimentStatus};
use super::{ExperimentPlan, ExperimentRunner, invalid};

/// `experiment_start` and `experiment_report` over `runner` (spec 3.5 §7.12): give them to
/// the Growth agent, whose `experiments` job binds them.
///
/// # Example
///
/// ```rust,ignore
/// let growth = HarnessAgent::builder("growth")
///     .website_back_office(&kit)
///     .tool(experiment_tools(runner.clone())[0].clone())
///     .build()?;
/// ```
pub fn experiment_tools(runner: Arc<ExperimentRunner>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ExperimentStartTool { runner: runner.clone() }),
        Arc::new(ExperimentReportTool { runner }),
    ]
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Replace {
    find: String,
    replace: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartArgs {
    id: String,
    page: String,
    primary_kpi: String,
    #[serde(default)]
    hypothesis: Option<String>,
    variant: Vec<Replace>,
    #[serde(default)]
    goal_pages: Vec<String>,
    #[serde(default)]
    item: Option<String>,
    #[serde(default)]
    guardrails: Vec<String>,
    #[serde(default)]
    allocation: Option<f64>,
}

/// `experiment_start`: starts a plan under the charter in force.
struct ExperimentStartTool {
    runner: Arc<ExperimentRunner>,
}

#[async_trait]
impl Tool for ExperimentStartTool {
    fn name(&self) -> &str {
        EXPERIMENT_START
    }

    fn description(&self) -> &str {
        "Starts a web experiment on one page: a variant made of literal replacements of the \
         page's HTML, the charter KPI it should move and the charter guardrails that stop it. \
         The next release serves the variant to a share of visitors."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Lowercase letters, digits and -; never reused" },
                "page": { "type": "string", "description": "The control page's URL path, such as /" },
                "primaryKpi": { "type": "string", "description": "The charter KPI id the variant should move" },
                "hypothesis": { "type": "string" },
                "variant": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": { "find": { "type": "string" }, "replace": { "type": "string" } },
                        "required": ["find", "replace"],
                        "additionalProperties": false
                    },
                    "minItems": 1
                },
                "goalPages": { "type": "array", "items": { "type": "string" } },
                "item": { "type": "string", "description": "The content item rendered as the control page" },
                "guardrails": { "type": "array", "items": { "type": "string" }, "description": "Charter guardrail ids; empty means every guardrail of the job" },
                "allocation": { "type": "number", "exclusiveMinimum": 0, "exclusiveMaximum": 1 }
            },
            "required": ["id", "page", "primaryKpi", "variant"],
            "additionalProperties": false
        }))
    }

    fn origin(&self) -> &str {
        BACK_OFFICE_ORIGIN
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let args: StartArgs = serde_json::from_value(args)
            .map_err(|e| invalid(format!("invalid arguments for experiment_start: {e}")))?;
        let mut plan = ExperimentPlan::new(args.id, args.page, args.primary_kpi);
        if let Some(hypothesis) = args.hypothesis {
            plan = plan.hypothesis(hypothesis);
        }
        for change in args.variant {
            plan = plan.replace(change.find, change.replace);
        }
        for page in args.goal_pages {
            plan = plan.goal_page(page);
        }
        if let Some(item) = args.item {
            plan = plan.item(item);
        }
        for guardrail in args.guardrails {
            plan = plan.guardrail(guardrail);
        }
        if let Some(share) = args.allocation {
            plan = plan.allocation(share);
        }
        let record = self.runner.start(plan).await?;
        Ok(json!({ "started": true, "experiment": summary(&record) }))
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReportArgs {
    #[serde(default)]
    id: Option<String>,
}

/// `experiment_report`: the site's experiments, or one, with their state and evidence.
struct ExperimentReportTool {
    runner: Arc<ExperimentRunner>,
}

#[async_trait]
impl Tool for ExperimentReportTool {
    fn name(&self) -> &str {
        EXPERIMENT_REPORT
    }

    fn description(&self) -> &str {
        "Reports the site's experiments, or the one named: whether each runs, its counts per \
         arm, its decision and the analysis behind it."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "additionalProperties": false
        }))
    }

    fn origin(&self) -> &str {
        BACK_OFFICE_ORIGIN
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let args: ReportArgs = if args.is_null() {
            ReportArgs::default()
        } else {
            serde_json::from_value(args)
                .map_err(|e| invalid(format!("invalid arguments for experiment_report: {e}")))?
        };
        let records = match args.id {
            Some(id) => self.runner.get(&id).await?.into_iter().collect(),
            None => self.runner.list().await?,
        };
        Ok(json!({ "experiments": records.iter().map(summary).collect::<Vec<_>>() }))
    }
}

/// What the model is told of an experiment: its plan's terms, where it stands, its counts and
/// its latest analysis.
fn summary(record: &ExperimentRecord) -> Value {
    let (status, decision) = match &record.status {
        ExperimentStatus::Running => ("running", Value::Null),
        ExperimentStatus::Stopped { decision, .. } => {
            ("stopped", serde_json::to_value(decision).unwrap_or(Value::Null))
        }
    };
    json!({
        "id": record.plan.id,
        "page": record.plan.page,
        "hypothesis": record.plan.hypothesis,
        "primaryKpi": record.plan.primary_kpi,
        "status": status,
        "decision": decision,
        "startedAt": record.started_at,
        "counts": record.counts,
        "latest": record.latest,
    })
}

#[cfg(test)]
mod tests {
    use adk_kernel::charter::InMemoryCharterStore;
    use adk_tool::SimpleToolContext;

    use super::super::InMemoryExperimentStore;
    use super::*;

    /// The tools are the catalogue's, under the back office's origin, and go through the
    /// runner: before the first charter commit nothing starts, and the report lists nothing.
    #[tokio::test]
    async fn the_tools_start_through_the_runner_and_report_its_records() {
        let runner = Arc::new(ExperimentRunner::new(
            uuid::Uuid::nil(),
            Arc::new(InMemoryCharterStore::new()),
            Arc::new(InMemoryExperimentStore::new()),
        ));
        let tools = experiment_tools(runner);
        let names: Vec<(&str, &str)> = tools.iter().map(|t| (t.name(), t.origin())).collect();
        assert_eq!(
            names,
            [(EXPERIMENT_START, BACK_OFFICE_ORIGIN), (EXPERIMENT_REPORT, BACK_OFFICE_ORIGIN)]
        );
        let ctx = Arc::new(SimpleToolContext::new("growth"));
        let plan = json!({
            "id": "headline",
            "page": "/",
            "primaryKpi": "signups",
            "variant": [{ "find": "<h1>Juniper</h1>", "replace": "<h1>Bread at 5am</h1>" }],
        });
        let refused = tools[0].execute(ctx.clone(), plan).await.unwrap_err();
        assert!(refused.to_string().contains("no committed charter"), "{refused}");
        let invalid = tools[0].execute(ctx.clone(), json!({ "id": "x" })).await.unwrap_err();
        assert!(invalid.to_string().contains("experiment_start"), "{invalid}");
        let report = tools[1].execute(ctx, json!({})).await.unwrap();
        assert_eq!(report, json!({ "experiments": [] }));
    }
}
