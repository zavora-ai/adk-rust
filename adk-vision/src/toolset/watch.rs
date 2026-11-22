//! `vision_watch`: watch conditions on the registered frame sources (spec 3.7 §7.8, §7.10).

use std::sync::Arc;
use std::time::Duration;

use adk_core::egress::ToolEgress;
use adk_core::{Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::Inner;
use crate::error::{Result, VisionError};
use crate::media::FrameSourceId;
use crate::stream::FrameSources;
use crate::watch::{WatchCondition, Zone, ZoneAnchor};

/// The name of the watch tool.
pub const VISION_WATCH_TOOL: &str = "vision_watch";

/// `vision_watch`: lists the frame sources an operator registered, and registers or removes
/// a watch condition on one of them (spec 3.7 §7.8). A registered condition streams the
/// source on the device; each event that passes keeps its frame and wakes the agent that
/// sleeps on the source. The agent names a source and never supplies one.
///
/// `Idempotent`: registering the same condition again, or removing one that is gone,
/// changes nothing.
pub(crate) struct VisionWatchTool {
    sources: FrameSources,
}

impl VisionWatchTool {
    pub(crate) fn new(sources: FrameSources) -> Self {
        Self { sources }
    }
}

fn source_arg(args: &Value) -> Result<FrameSourceId> {
    args.get("source")
        .and_then(Value::as_str)
        .map(|source| FrameSourceId(source.to_string()))
        .ok_or_else(|| VisionError::InvalidInput("`source` names the frame source to watch".into()))
}

fn name_arg(args: &Value) -> Result<String> {
    args.get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| VisionError::InvalidInput("`name` names the watch condition".into()))
}

/// The condition a `start` call describes, with [`WatchCondition`]'s defaults for what it
/// leaves out.
pub(crate) fn condition_arg(args: &Value) -> Result<WatchCondition> {
    let labels: Vec<String> = match args.get("labels") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_string).ok_or_else(|| {
                    VisionError::InvalidInput("`labels` is a list of label names".into())
                })
            })
            .collect::<Result<_>>()?,
        Some(_) => {
            return Err(VisionError::InvalidInput("`labels` is a list of label names".into()));
        }
    };
    let mut condition = WatchCondition::new(name_arg(args)?, labels);
    if let Some(score) = args.get("minScore").and_then(Value::as_f64) {
        condition = condition.with_min_score(score as f32);
    }
    if let Some(frames) = args.get("minFrames") {
        let frames = frames
            .as_u64()
            .and_then(|frames| u32::try_from(frames).ok())
            .ok_or_else(|| VisionError::InvalidInput("`minFrames` is a whole number".into()))?;
        condition = condition.with_min_frames(frames);
    }
    if let Some(seconds) = args.get("cooldownSeconds") {
        let seconds =
            seconds.as_f64().filter(|s| s.is_finite() && *s >= 0.0 && *s <= 86_400.0).ok_or_else(
                || VisionError::InvalidInput("`cooldownSeconds` is 0 to 86400 seconds".into()),
            )?;
        condition = condition.with_cooldown(Duration::from_secs_f64(seconds));
    }
    if let Some(zone) = args.get("zone").filter(|zone| !zone.is_null()) {
        let points: Vec<[f32; 2]> = serde_json::from_value(zone.clone()).map_err(|_| {
            VisionError::InvalidInput(
                "`zone` is a polygon: a list of [x, y] points in the frame's pixels".into(),
            )
        })?;
        let mut zone = Zone::new(points)?;
        if let Some(anchor) = args.get("zoneAnchor").and_then(Value::as_str) {
            zone = zone.with_anchor(match anchor {
                "center" => ZoneAnchor::Center,
                "bottomCenter" => ZoneAnchor::BottomCenter,
                other => {
                    return Err(VisionError::InvalidInput(format!(
                        "`zoneAnchor` is `center` or `bottomCenter`, not `{other}`"
                    )));
                }
            });
        }
        condition = condition.with_zone(zone);
    }
    condition.validate()?;
    Ok(condition)
}

#[async_trait]
impl Tool for VisionWatchTool {
    fn name(&self) -> &str {
        VISION_WATCH_TOOL
    }

    fn description(&self) -> &str {
        "Watches a registered camera, screen or browser stream on this computer: `list` the sources and their watches, `start` a watch condition on a source, or `stop` one. A started condition wakes the agent sleeping on the source when a tracked object with one of its labels meets the score in enough consecutive frames; the wake names the kept frame by digest, which the other vision tools load."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["list", "start", "stop"], "description": "What to do." },
                "source": { "type": "string", "description": "The frame source, as `list` names it. Required for `start` and `stop`." },
                "name": { "type": "string", "description": "The watch condition's name, [A-Za-z0-9._-], at most 64 characters: part of every wake's key. Required for `start` and `stop`." },
                "labels": { "type": "array", "items": { "type": "string" }, "description": "The labels that count, from the source's labels; empty for any." },
                "minScore": { "type": "number", "description": "The lowest detector score that counts, 0 to 1; 0.5 by default." },
                "minFrames": { "type": "integer", "description": "Consecutive frames a tracked object must meet the condition in; 3 by default." },
                "zone": { "type": "array", "items": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 }, "description": "Only this polygon of the frame: [x, y] points in the frame's pixels." },
                "zoneAnchor": { "type": "string", "enum": ["center", "bottomCenter"], "description": "Which point of an object must lie in the zone; its centre by default." },
                "cooldownSeconds": { "type": "number", "description": "The least time between two wakes of this condition; none by default." }
            },
            "required": ["action"],
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Idempotent
    }

    fn egress(&self, _args: &Value) -> ToolEgress {
        ToolEgress::None
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let action = args.get("action").and_then(Value::as_str).unwrap_or_default();
        let cx = Inner::cx_for(ctx.as_ref());
        let answer = match action {
            "list" => json!({ "sources": self.sources.statuses() }),
            "start" => {
                let source = source_arg(&args)?;
                let condition = condition_arg(&args)?;
                let outcome = self.sources.start_watch(&cx, &source, condition).await?;
                serde_json::to_value(outcome)
                    .map_err(|err| VisionError::InvalidInput(err.to_string()))?
            }
            "stop" => {
                let source = source_arg(&args)?;
                let name = name_arg(&args)?;
                let outcome = self.sources.stop_watch(&cx, &source, &name).await?;
                serde_json::to_value(outcome)
                    .map_err(|err| VisionError::InvalidInput(err.to_string()))?
            }
            other => {
                return Err(VisionError::InvalidInput(format!(
                    "`action` is `list`, `start` or `stop`, not `{other}`"
                ))
                .into());
            }
        };
        Ok(answer)
    }
}
