//! The answer record each perception call leaves beside its image, for the annotated viewer
//! (spec 3.7 §8.2, X12).
//!
//! A tool's response is what the model reads, so it stays compact: boxes, labels and scores.
//! What a person needs to look at the answer — every region with its shape, the backend, the
//! coordinate frame and the transform chain, the page's blocks in reading order, and each
//! backend call's duration and cost — is saved as a JSON artifact in the session, named
//! `vision-answer-<call id>.json`, beside the image the media store keeps as
//! `vision-<hex>.<ext>`. Surfaces read it without depending on this crate.
//!
//! | Key | Holds |
//! |-----|-------|
//! | `kind`, `version` | `adkVisionAnswer`, `1` |
//! | `tool`, `callId`, `invocationId`, `agent`, `at` | The call and the run that made it |
//! | `image` | The source image: digest, size, MIME type, artifact, the name it was called by, provenance |
//! | `args` | The call's arguments |
//! | `regionSet` | The answer as a [`RegionSet`](crate::RegionSet), in source pixels with each region's origin |
//! | `page` | `vision_read` only: the blocks (kind, text, confidence, region) and the reading order |
//! | `answer` | `vision_describe` only: the text and the structured value |
//! | `calls` | Each backend call: backend, locality, egress digests, units, cost, regions, duration, escalation |
//! | `models` | Each manifest the regions name: revision and licence for a built-in one |
//!
//! Records carry regions and digests, never pixels (V-I15).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use adk_core::{Artifacts, Part, ToolContext};
use serde::Serialize;
use serde_json::{Value, json};

use crate::geometry::{Region, RegionSet};
use crate::manifest::{ManifestSet, ModelSource};
use crate::media::ImageRef;
use crate::runtime::CallRecord;
use crate::traits::DocumentPage;

/// The artifact name prefix of an answer record.
pub const ANSWER_ARTIFACT_PREFIX: &str = "vision-answer-";

/// What a perception call answered, before it is written as a record.
pub(crate) struct Answer {
    pub(crate) regions: Vec<Region>,
    pub(crate) page: Option<DocumentPage>,
    pub(crate) described: Option<Value>,
    pub(crate) calls: Vec<CallRecord>,
}

impl Answer {
    /// A detection's or a segmentation's answer.
    pub(crate) fn regions(regions: Vec<Region>, calls: Vec<CallRecord>) -> Self {
        Self { regions, page: None, described: None, calls }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelFacts {
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    licence: Option<String>,
    built_in: bool,
}

/// The artifact name of the record for call `call_id`: characters outside `[A-Za-z0-9_-]`
/// become `_`, so any call id makes one file name.
///
/// # Example
///
/// ```
/// assert_eq!(adk_vision::answer_artifact_name("call/7"), "vision-answer-call_7.json");
/// ```
pub fn answer_artifact_name(call_id: &str) -> String {
    let safe: String = call_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    format!("{ANSWER_ARTIFACT_PREFIX}{safe}.json")
}

fn builtin_manifests() -> Option<&'static ManifestSet> {
    static BUILTIN: OnceLock<Option<ManifestSet>> = OnceLock::new();
    BUILTIN.get_or_init(|| ManifestSet::builtin().ok()).as_ref()
}

fn models_of(regions: &[Region]) -> BTreeMap<String, ModelFacts> {
    let mut models = BTreeMap::new();
    for region in regions {
        let Some(id) = &region.origin.manifest else { continue };
        let known = builtin_manifests().and_then(|set| set.get(id));
        let revision = known.and_then(|manifest| match &manifest.source {
            ModelSource::HuggingFace { revision, .. }
            | ModelSource::ModelScope { revision, .. } => Some(revision.clone()),
            _ => None,
        });
        models.entry(id.clone()).or_insert(ModelFacts {
            revision,
            licence: known.map(|manifest| manifest.licence.id.clone()),
            built_in: known.is_some(),
        });
    }
    models
}

/// The record of `answer` for the call `ctx` runs, over `image` called by `name` with `args`.
pub(crate) fn record(
    ctx: &dyn ToolContext,
    tool: &str,
    image: &ImageRef,
    name: &str,
    args: &Value,
    answer: &Answer,
) -> Value {
    let mut regions = answer.regions.clone();
    let page = answer.page.as_ref().map(|page| {
        // The blocks' regions are the answer's regions; words and cells stay with the run.
        regions = page.blocks.iter().map(|block| block.region.clone()).collect();
        json!({
            "blocks": page.blocks.iter().map(|block| {
                let mut entry = json!({ "kind": block.kind, "text": block.text });
                if let Some(confidence) = block.confidence {
                    entry["confidence"] = json!(confidence);
                }
                entry
            }).collect::<Vec<_>>(),
            "readingOrder": page.reading_order,
        })
    });
    let models = models_of(&regions);
    let set = RegionSet::new(image.digest, image.width, image.height, regions);
    let mut record = json!({
        "kind": "adkVisionAnswer",
        "version": 1,
        "tool": tool,
        "callId": ctx.function_call_id(),
        "invocationId": ctx.invocation_id(),
        "agent": ctx.agent_name(),
        "at": chrono::Utc::now(),
        "image": {
            "digest": image.digest,
            "width": image.width,
            "height": image.height,
            "mimeType": image.mime_type,
            "artifact": image.artifact.file_name,
            "name": name,
            "provenance": image.provenance,
        },
        "args": args,
        "regionSet": set,
        "calls": answer.calls,
        "models": models,
    });
    if let Some(page) = page {
        record["page"] = page;
    }
    if let Some(described) = &answer.described {
        record["answer"] = described.clone();
    }
    record
}

/// Saves `record` as the answer artifact of `call_id`. A record that cannot be saved is
/// logged and the call's answer stands: the viewer is a view of the run, not part of it.
pub(crate) async fn save(artifacts: &dyn Artifacts, call_id: &str, record: &Value) {
    let name = answer_artifact_name(call_id);
    let data = match serde_json::to_vec(record) {
        Ok(data) => data,
        Err(error) => {
            tracing::warn!(error = %error, artifact.name = %name, "answer record not serialised");
            return;
        }
    };
    let part = Part::InlineData {
        mime_type: "application/json".into(),
        data,
        uri: None,
        annotations: None,
    };
    if let Err(error) = artifacts.save(&name, &part).await {
        tracing::warn!(error = %error, artifact.name = %name, "answer record not saved");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_names_are_one_safe_file_name() {
        assert_eq!(answer_artifact_name("c-31"), "vision-answer-c-31.json");
        assert_eq!(answer_artifact_name("a/../b c"), "vision-answer-a____b_c.json");
    }
}
