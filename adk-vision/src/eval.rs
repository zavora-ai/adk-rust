//! Vision backends as eval agents (spec 3.7 §7.16).
//!
//! [`VisionEvalAgent`] answers `adk-eval` cases with one [`VisionRuntime`] task and reports
//! the answer as the matching governed tool would — a `vision_detect`, `vision_segment`,
//! `vision_read` or `vision_describe` call and its response — so `adk-eval`'s vision
//! criteria score a bare backend exactly as they score an agent that called the tool. It
//! runs in process as an `adk_core::Agent`, or behind `cargo adk eval --agent-cmd` through
//! [`serve_eval_protocol`].
//!
//! | Task | Tool | The turn's text is |
//! |------|------|--------------------|
//! | [`EvalTask::Detect`] | `vision_detect` | comma-separated classes (empty: every class), or concepts |
//! | [`EvalTask::Segment`] | `vision_segment` | a JSON array of [`SegmentPrompt`]s, else one concept |
//! | [`EvalTask::Read`] | `vision_read` | ignored |
//! | [`EvalTask::Describe`] | `vision_describe` | the question |
//!
//! Responses carry what the scorers need beyond the governed tools' compact form: masks for
//! segmentation, blocks in reading order, and each call's units, cost and wall time.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use adk_vision::eval::{EvalTask, VisionEvalAgent};
//! use adk_vision::{
//!     BBox, BackendDescriptor, CoordFrame, DecodedImage, DetectQuery, Detector, Locality,
//!     RawRegion, SentImage, SourceImage, VisionCx, VisionRuntime,
//! };
//!
//! struct Page(BackendDescriptor);
//!
//! #[async_trait::async_trait]
//! impl Detector for Page {
//!     fn descriptor(&self) -> &BackendDescriptor {
//!         &self.0
//!     }
//!     async fn detect(
//!         &self,
//!         _cx: &VisionCx,
//!         image: &SentImage,
//!         _query: &DetectQuery,
//!     ) -> adk_vision::Result<Vec<RawRegion>> {
//!         let (w, h) = image.size();
//!         Ok(vec![RawRegion::boxed(BBox::new(0.0, 0.0, w as f32, h as f32), "page", 0.9)])
//!     }
//! }
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let page = Page(BackendDescriptor::new("page", Locality::InProcess, CoordFrame::SentPixels));
//! let runtime = VisionRuntime::builder().detector(Arc::new(page)).build().unwrap();
//! let agent = VisionEvalAgent::new(Arc::new(runtime), EvalTask::Detect { concepts: false });
//!
//! let image = SourceImage::from_pixels(DecodedImage::filled(40, 30, [255; 4])).unwrap();
//! let answer = agent.answer(&VisionCx::new(), &[image], "page").await.unwrap();
//! assert_eq!(answer.tool, "vision_detect");
//! assert_eq!(answer.response["regions"][0]["box"], serde_json::json!([0.0, 0.0, 40.0, 30.0]));
//! assert_eq!(answer.response["calls"][0]["backend"], "page");
//! # });
//! ```

use std::sync::Arc;

use adk_core::{
    AdkError, Agent, Content, Event, EventStream, FunctionResponseData, InvocationContext, Part,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::error::{Result, VisionError};
use crate::geometry::{BBox, Region, Shape};
use crate::media::{DecodeLimits, SourceImage};
use crate::runtime::{CallRecord, VisionRuntime};
use crate::traits::{Block, DescribeSpec, DetectQuery, ReadOptions, SegmentPrompt};

/// The runtime task a [`VisionEvalAgent`] answers every turn with.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum EvalTask {
    /// Detection; the turn's text lists classes, or concepts when `concepts` is set.
    Detect {
        /// Send the names as open-vocabulary concepts rather than closed-set classes.
        concepts: bool,
    },
    /// Segmentation; the turn's text is a JSON array of prompts, or one concept.
    Segment,
    /// Reading a page.
    Read {
        /// Ask for Markdown too.
        markdown: bool,
        /// Report `Label: value` lines as fields.
        fields: bool,
    },
    /// Describing; the turn's text is the question.
    Describe {
        /// Ask for regions.
        want_regions: bool,
        /// A JSON Schema the answer's value must match; its fields are scored.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        schema: Option<Value>,
    },
}

impl EvalTask {
    /// The governed tool whose response this task's answer mirrors.
    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::Detect { .. } => "vision_detect",
            Self::Segment => "vision_segment",
            Self::Read { .. } => "vision_read",
            Self::Describe { .. } => "vision_describe",
        }
    }
}

/// One answered turn: the tool call it stands for and the tool's response.
#[derive(Clone, Debug, PartialEq)]
pub struct EvalAnswer {
    /// The governed tool's name.
    pub tool: &'static str,
    /// The call's arguments.
    pub args: Value,
    /// The tool's response, with masks, reading order and call records.
    pub response: Value,
    /// Every backend call the runtime made.
    pub calls: Vec<CallRecord>,
}

/// A [`VisionRuntime`] task as an eval agent; see the [module docs](self).
pub struct VisionEvalAgent {
    runtime: Arc<VisionRuntime>,
    task: EvalTask,
    name: String,
    limits: DecodeLimits,
    cx: VisionCx,
}

impl VisionEvalAgent {
    /// An agent answering every turn with `task` on `runtime`.
    pub fn new(runtime: Arc<VisionRuntime>, task: EvalTask) -> Self {
        Self {
            runtime,
            task,
            name: "vision_eval".to_string(),
            limits: DecodeLimits::default(),
            cx: VisionCx::new(),
        }
    }

    /// Names the agent in its events.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Caps the images [`serve_eval_protocol`] and [`Agent::run`] decode.
    pub fn with_decode_limits(mut self, limits: DecodeLimits) -> Self {
        self.limits = limits;
        self
    }

    /// The context in-process and protocol turns run in: its budget, spend ledger,
    /// governance and cancellation apply to every call.
    pub fn with_context(mut self, cx: VisionCx) -> Self {
        self.cx = cx;
        self
    }

    /// The task.
    pub fn task(&self) -> &EvalTask {
        &self.task
    }

    /// The runtime.
    pub fn runtime(&self) -> &Arc<VisionRuntime> {
        &self.runtime
    }

    /// Answers one turn: `images` are the turn's images, `text` its text.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for a turn without an image, or a detect, segment or
    /// read turn with more than one; otherwise the runtime's error.
    pub async fn answer(
        &self,
        cx: &VisionCx,
        images: &[SourceImage],
        text: &str,
    ) -> Result<EvalAnswer> {
        let Some(first) = images.first() else {
            return Err(VisionError::InvalidInput(
                "the turn shows no image; add an {\"image\": {\"path\", \"digest\"}} part".into(),
            ));
        };
        let single = || {
            if images.len() == 1 {
                Ok(first)
            } else {
                Err(VisionError::InvalidInput(format!(
                    "{} takes one image per turn, and the turn shows {}",
                    self.task.tool_name(),
                    images.len()
                )))
            }
        };
        let (width, height) = first.size();
        let image = first.digest();
        let tool = self.task.tool_name();
        let (args, response, calls) = match &self.task {
            EvalTask::Detect { concepts } => {
                let names: Vec<String> = text
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
                    .collect();
                let (key, query) = if *concepts {
                    ("concepts", DetectQuery::Concepts(names.clone()))
                } else {
                    ("classes", DetectQuery::Classes(names.clone()))
                };
                let out = self.runtime.detect(cx, single()?, &query).await?;
                let response = json!({
                    "image": image, "width": width, "height": height,
                    "regions": out.value.iter().map(region_json).collect::<Vec<_>>(),
                    "calls": calls_json(&out.calls),
                });
                (json!({ key: names }), response, out.calls)
            }
            EvalTask::Segment => {
                let prompts = serde_json::from_str::<Vec<SegmentPrompt>>(text.trim())
                    .unwrap_or_else(|_| vec![SegmentPrompt::Concept { text: text.trim().into() }]);
                let out = self.runtime.segment(cx, single()?, &prompts).await?;
                let response = json!({
                    "image": image, "width": width, "height": height,
                    "regions": out.value.iter().map(region_json).collect::<Vec<_>>(),
                    "calls": calls_json(&out.calls),
                });
                (json!({ "prompts": prompts }), response, out.calls)
            }
            EvalTask::Read { markdown, fields } => {
                let options = ReadOptions { markdown: *markdown, ..ReadOptions::default() };
                let out = self.runtime.read(cx, single()?, &options).await?;
                let page = &out.value;
                let mut order = page.reading_order.clone();
                order.extend((0..page.blocks.len()).filter(|i| !page.reading_order.contains(i)));
                let blocks: Vec<&Block> =
                    order.iter().filter_map(|&i| page.blocks.get(i)).collect();
                let mut response = json!({
                    "image": image,
                    "blocks": blocks.iter().map(|block| {
                        let mut entry = json!({
                            "kind": block.kind,
                            "text": block.text,
                            "box": block.region.shape.bbox().map(box_json),
                        });
                        if let Some(confidence) = block.confidence {
                            entry["confidence"] = json!(confidence);
                        }
                        entry
                    }).collect::<Vec<_>>(),
                    "markdown": page.markdown,
                    "calls": calls_json(&out.calls),
                });
                if *fields {
                    let found: serde_json::Map<String, Value> = blocks
                        .iter()
                        .flat_map(|block| block.text.lines())
                        .filter_map(label_value)
                        .map(|(label, value)| (label.to_string(), json!(value)))
                        .collect();
                    response["fields"] = Value::Object(found);
                }
                (json!({ "markdown": markdown }), response, out.calls)
            }
            EvalTask::Describe { want_regions, schema } => {
                let mut spec = DescribeSpec::new(text);
                spec.output_schema = schema.clone();
                spec.want_regions = *want_regions;
                let refs: Vec<&SourceImage> = images.iter().collect();
                let out = self.runtime.describe(cx, &refs, &spec).await?;
                let answer = &out.value;
                let response = json!({
                    "image": image,
                    "text": answer.text,
                    "value": answer.value,
                    "confidence": answer.confidence,
                    "regions": answer.regions.iter().map(region_json).collect::<Vec<_>>(),
                    "calls": calls_json(&out.calls),
                });
                (json!({ "question": text }), response, out.calls)
            }
        };
        Ok(EvalAnswer { tool, args, response, calls })
    }
}

/// A `Label: value` line: a label of letters, spaces, `-` or `_`, at most 40 characters.
fn label_value(line: &str) -> Option<(&str, &str)> {
    let (label, value) = line.split_once(':')?;
    let (label, value) = (label.trim(), value.trim());
    let is_label = !label.is_empty()
        && label.len() <= 40
        && label.chars().all(|c| c.is_alphabetic() || matches!(c, ' ' | '-' | '_'));
    (is_label && !value.is_empty()).then_some((label, value))
}

fn box_json(bbox: BBox) -> Value {
    json!([bbox.x0, bbox.y0, bbox.x1, bbox.y1])
}

fn region_json(region: &Region) -> Value {
    let mut entry = json!({ "box": region.shape.bbox().map(box_json) });
    if let Some(label) = &region.label {
        entry["label"] = json!(label);
    }
    if let Some(score) = region.score {
        entry["score"] = json!(score);
    }
    if let Shape::Mask(mask) = &region.shape {
        entry["mask"] = json!(mask);
    }
    if let Some(from) = &region.origin.escalated_from {
        entry["escalatedFrom"] = json!(from.backend);
    }
    entry
}

fn calls_json(calls: &[CallRecord]) -> Vec<Value> {
    calls
        .iter()
        .map(|call| {
            json!({
                "backend": call.backend,
                "remote": call.locality.is_remote(),
                "units": call.units,
                "costMicroUsd": call.cost_micro_usd,
                "durationMs": call.duration_ms,
                "egress": call.egress,
            })
        })
        .collect()
}

#[async_trait]
impl Agent for VisionEvalAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "answers eval cases with one vision runtime task"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        let content = ctx.user_content();
        let mut images = Vec::new();
        for part in &content.parts {
            if let Part::InlineData { data, .. } = part {
                images.push(SourceImage::decode(data, &self.limits)?);
            }
        }
        let text: String = content.parts.iter().filter_map(Part::text).collect();
        let answer = self.answer(&self.cx, &images, &text).await.map_err(AdkError::from)?;
        let event = |content: Content| {
            let mut event = Event::new(ctx.invocation_id());
            event.author = self.name.clone();
            event.llm_response.content = Some(content);
            Ok(event)
        };
        let mut call = Content::new("model");
        call.parts.push(Part::FunctionCall {
            name: answer.tool.to_string(),
            args: answer.args,
            id: None,
            thought_signature: None,
        });
        let mut result = Content::new("function");
        result.parts.push(Part::FunctionResponse {
            function_response: FunctionResponseData::new(answer.tool, answer.response),
            id: None,
            annotations: None,
        });
        Ok(Box::pin(futures::stream::iter([event(call), event(result)])))
    }
}

/// One `cargo adk eval` request line.
#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    user_text: String,
    #[serde(default)]
    images: Vec<RequestImage>,
}

#[derive(Deserialize)]
struct RequestImage {
    path: std::path::PathBuf,
    digest: String,
}

/// Serves `agent` over the `cargo adk eval` agent protocol until `input` ends: one JSON
/// request per line in, one response line out. Each image is read from its `path` and
/// checked against its `digest` before it is decoded; a failed turn answers `{"error"}`
/// and the loop goes on.
///
/// ```text
/// → {"case_id":"shapes","turn":0,"user_text":"","session_id":"…","images":[{"path":"…","digest":"sha256:…"}]}
/// ← {"tool_calls":[{"name":"vision_detect","args":{"classes":[]},"response":{"regions":[…],…}}]}
/// ```
///
/// A binary for `--agent-cmd` builds its runtime and calls this with stdin and stdout.
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use adk_vision::eval::{EvalTask, VisionEvalAgent, serve_eval_protocol};
/// use adk_vision::VisionRuntime;
///
/// # async fn serve(runtime: VisionRuntime) -> std::io::Result<()> {
/// let agent = VisionEvalAgent::new(Arc::new(runtime), EvalTask::Segment);
/// let input: &[u8] = b"";
/// serve_eval_protocol(&agent, input, Vec::new()).await
/// # }
/// ```
///
/// # Errors
///
/// Returns the I/O error that ended reading `input` or writing `output`.
pub async fn serve_eval_protocol<R, W>(
    agent: &VisionEvalAgent,
    input: R,
    mut output: W,
) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = input.lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match answer_line(agent, &line).await {
            Ok(answer) => json!({ "tool_calls": [{
                "name": answer.tool,
                "args": answer.args,
                "response": answer.response,
            }]}),
            Err(e) => json!({ "error": e.to_string() }),
        };
        let mut text = reply.to_string();
        text.push('\n');
        output.write_all(text.as_bytes()).await?;
        output.flush().await?;
    }
    Ok(())
}

async fn answer_line(agent: &VisionEvalAgent, line: &str) -> Result<EvalAnswer> {
    let request: Request = serde_json::from_str(line)
        .map_err(|e| VisionError::InvalidInput(format!("not a cargo adk eval request: {e}")))?;
    let mut images = Vec::with_capacity(request.images.len());
    for image in &request.images {
        let bytes = std::fs::read(&image.path)
            .map_err(|e| VisionError::io("could not read", &image.path, e))?;
        let want = Digest::from_hex(&image.digest)?;
        let actual = Digest::of(&bytes);
        if actual != want {
            return Err(VisionError::InvalidInput(format!(
                "{} is {actual}, not the {want} the eval case pins",
                image.path.display()
            )));
        }
        images.push(SourceImage::decode(&bytes, &agent.limits)?);
    }
    agent.answer(&agent.cx, &images, &request.user_text).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_value_lines() {
        assert_eq!(label_value("Name: Amara Okafor"), Some(("Name", "Amara Okafor")));
        assert_eq!(label_value("Due: 2026-12-19"), Some(("Due", "2026-12-19")));
        assert_eq!(label_value("Ship-to: Dock 4"), Some(("Ship-to", "Dock 4")));
        assert_eq!(label_value("12:30 pm"), None);
        assert_eq!(label_value("Total:"), None);
        assert_eq!(label_value("No colon here"), None);
    }

    #[test]
    fn tasks_name_their_tools_and_round_trip() {
        let tasks = [
            EvalTask::Detect { concepts: true },
            EvalTask::Segment,
            EvalTask::Read { markdown: false, fields: true },
            EvalTask::Describe { want_regions: true, schema: None },
        ];
        let names: Vec<&str> = tasks.iter().map(EvalTask::tool_name).collect();
        assert_eq!(names, ["vision_detect", "vision_segment", "vision_read", "vision_describe"]);
        for task in tasks {
            let json = serde_json::to_value(&task).unwrap();
            assert_eq!(serde_json::from_value::<EvalTask>(json).unwrap(), task);
        }
        assert_eq!(
            serde_json::to_value(EvalTask::Read { markdown: true, fields: false }).unwrap(),
            json!({"kind": "read", "markdown": true, "fields": false})
        );
    }
}
