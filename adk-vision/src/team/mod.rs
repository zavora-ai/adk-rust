//! Teams: perceiver, reasoner, verifier (spec 3.7 §7.12, feature `team`).
//!
//! Three roles, each enforced where it is built, and a coordinator that escalates a
//! disagreement to a person:
//!
//! | Role | Model | Tools | Sees | Construction refuses |
//! |------|-------|-------|------|----------------------|
//! | [`Perceiver`] | On the device, or none | A local [`VisionToolset`](crate::VisionToolset) and tools that declare no network use | Full images on the device | A remote model, a runtime with a remote backend, a tool that declares any network use |
//! | [`Reasoner`] | Any; usually a cloud model | None | A `RegionSet`, measurements and crops the egress gate released | A remote model without an egress gate |
//! | [`Verifier`] | Any; by default another provider than the reasoner's | Local perception | The claim, its regions and the source image on the device | A runtime with a remote backend |
//!
//! [`VisionTeam`] runs them on one image: the perceiver finds regions on the device; each
//! region whose score is ambiguous goes, as a redacted crop, to the reasoner; the verifier
//! re-checks the reasoner's claim against the pixels. When the two disagree, the team holds
//! one governed call, [`DISAGREEMENT_TOOL`], with both opinions in its arguments and the
//! frame and the crop the reasoner saw as its evidence, so every surface draws one card with
//! the image (spec 3.7 S15). A person's approval accepts the reasoner's verdict and a denial
//! keeps the verifier's.
//!
//! Each role is also an [`Agent`](adk_core::Agent), so it binds to a member of
//! `adk_agent::TeamArchitectureTemplate::PerceiveReasonVerify` and is held to that member's
//! edge contract.
//!
//! Using a different provider for the verifier is a default, not a rule:
//! [`VisionTeamBuilder::allow_same_provider`] lifts it.

mod coordinator;
mod roles;

pub use coordinator::{
    DISAGREEMENT_TOOL, Finding, FindingSource, Inspection, RELEASE_TOOL, VisionTeam,
    VisionTeamBuilder,
};
pub use roles::{
    LocalOnlyTool, Perceiver, PerceiverBuilder, ReasonRequest, Reasoned, Reasoner,
    ReasonerBuilder, RegionCheck, Verified, Verifier, VerifierBuilder, VerifyRequest,
};

use std::sync::Arc;

use adk_core::{
    AdkError, Content, ErrorCategory, ErrorComponent, Event, InvocationContext, ModelCallScope,
    Part,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::error::{Result, VisionError};
use crate::geometry::RegionSet;
use crate::media::{DecodeLimits, MediaScope, SourceImage};

/// What a reasoner or verifier answers about one claim: a verdict, a confidence, the regions
/// it rests on and why, with the member and provider that answered.
///
/// Its JSON form matches `adk_agent::opinion_schema`.
///
/// # Example
///
/// ```
/// use adk_vision::team::Opinion;
///
/// let reasoner = Opinion::new("defect", 0.8).with_label("scratch");
/// let verifier = Opinion::new("No_Defect", 0.7);
/// assert!(!reasoner.agrees_with(&verifier));
/// assert!(verifier.agrees_with(&Opinion::new("no_defect", 0.2)));
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Opinion {
    /// One of the verdicts the team asks for.
    pub verdict: String,
    /// The class the opinion is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Confidence in `[0, 1]`.
    pub confidence: f32,
    /// The regions the opinion rests on, in source pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regions: Option<RegionSet>,
    /// Why, in one or two sentences.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rationale: String,
    /// The member that answered.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub member: String,
    /// The provider that answered, `local` for an on-device check.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
}

impl Opinion {
    /// An opinion with `verdict` at `confidence`.
    pub fn new(verdict: impl Into<String>, confidence: f32) -> Self {
        Self {
            verdict: verdict.into(),
            label: None,
            confidence: confidence.clamp(0.0, 1.0),
            regions: None,
            rationale: String::new(),
            member: String::new(),
            provider: String::new(),
        }
    }

    /// Names the class the opinion is about.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Says why.
    #[must_use]
    pub fn with_rationale(mut self, rationale: impl Into<String>) -> Self {
        self.rationale = rationale.into();
        self
    }

    /// Whether both verdicts are the same, ignoring case and surrounding space.
    pub fn agrees_with(&self, other: &Opinion) -> bool {
        normalize_verdict(&self.verdict) == normalize_verdict(&other.verdict)
    }
}

pub(crate) fn normalize_verdict(verdict: &str) -> String {
    verdict.trim().to_ascii_lowercase().replace([' ', '-'], "_")
}

/// Why a team role or a team could not be built.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TeamRoleError {
    /// A perceiver's model sends its requests off the machine.
    #[error(
        "the {role} '{member}' would send what it sees to the remote model '{model}'; give it a \
         model that runs on the device (adk-mistralrs, or a server at a loopback address), or \
         none"
    )]
    RemoteModel {
        /// The role.
        role: &'static str,
        /// The member.
        member: String,
        /// The model.
        model: String,
    },
    /// A tool a perceiver or verifier would hold declares network use.
    #[error(
        "the {role} '{member}' cannot hold `{tool}`: it declares {egress}, and a {role} holds only \
         tools that declare no network use (ToolEgress::None)"
    )]
    RemoteTool {
        /// The role.
        role: &'static str,
        /// The member.
        member: String,
        /// The tool.
        tool: String,
        /// What the tool declares.
        egress: String,
    },
    /// A runtime a perceiver or verifier perceives through has a remote backend.
    #[error(
        "the {role} '{member}' perceives on the device, and its runtime has the remote backend \
         `{backend}` ({hosts}); build its runtime from on-device backends only"
    )]
    RemoteBackend {
        /// The role.
        role: &'static str,
        /// The member.
        member: String,
        /// The backend.
        backend: String,
        /// Its hosts.
        hosts: String,
    },
    /// A vision tool was handed over as a plain tool, so its runtime cannot be checked.
    #[error(
        "give the {role} '{member}' its vision tools through `toolset`, so their runtime can be \
         checked; `{tool}` was given as a plain tool"
    )]
    UncheckedVisionTool {
        /// The role.
        role: &'static str,
        /// The member.
        member: String,
        /// The tool.
        tool: String,
    },
    /// A required part of a role or a team is missing.
    #[error("{0}")]
    Missing(String),
    /// The verifier runs on the reasoner's provider.
    #[error(
        "the verifier and the reasoner both run on `{provider}`; by default the verifier uses \
         another provider so the check is independent. Give the verifier another provider or a \
         local check, or call VisionTeamBuilder::allow_same_provider"
    )]
    SameProvider {
        /// The shared provider.
        provider: String,
    },
}

impl From<TeamRoleError> for AdkError {
    fn from(error: TeamRoleError) -> Self {
        AdkError::new(
            ErrorComponent::Agent,
            ErrorCategory::InvalidInput,
            "vision.team.invalid_role",
            error.to_string(),
        )
    }
}

/// The provider a model runs on, for the verifier's provider default: `explicit` when given,
/// else the vendor its id names, else the model id itself, so two unrecognised models count
/// as the same provider only when they are the same model.
///
/// # Example
///
/// ```
/// use adk_vision::team::provider_of;
///
/// assert_eq!(provider_of("claude-haiku-5-5", None), "anthropic");
/// assert_eq!(provider_of("nvidia/nemotron-3.5-super-vl-120b-a12b", None), "nvidia/nemotron-3.5-super-vl-120b-a12b");
/// assert_eq!(provider_of("nvidia/nemotron-3.5-super-vl-120b-a12b", Some("nvidia")), "nvidia");
/// ```
pub fn provider_of(model: &str, explicit: Option<&str>) -> String {
    if let Some(explicit) = explicit {
        return explicit.to_string();
    }
    match adk_core::vendor_for_model(model) {
        "unknown" => model.to_string(),
        vendor => vendor.to_string(),
    }
}

/// The context a role runs vision calls under inside an agent run: the run's metering scope,
/// the session's media scope, the acting agent and the run's audit trail.
pub(crate) fn cx_for(ctx: &dyn InvocationContext) -> VisionCx {
    let scope = ModelCallScope::for_run(ctx.run_config(), ctx.app_name(), ctx.agent_name());
    let acting = ctx.acting_context();
    let mut cx = VisionCx::new()
        .with_scope(scope)
        .with_media_scope(MediaScope::new(ctx.app_name(), ctx.user_id(), ctx.session_id()))
        .with_acting(acting.clone());
    if let Some(trail) = ctx.run_config().audit.clone() {
        cx = cx.with_audit(
            trail,
            adk_core::audit_log::AuditActor::new(&acting, ctx.invocation_id()),
        );
    }
    cx
}

/// The request an agent form of a role was sent: the JSON its edge contract admits, or a
/// bare image name.
pub(crate) fn request_of(ctx: &dyn InvocationContext) -> Value {
    let text: String = ctx
        .user_content()
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    extract_json(&text).unwrap_or_else(|| serde_json::json!({ "image": text.trim() }))
}

/// Loads an image a request names, by `sha256:` digest of an image the session holds, or by
/// artifact name.
pub(crate) async fn load_image(ctx: &dyn InvocationContext, image: &str) -> Result<SourceImage> {
    let bytes = load_bytes(ctx, image).await?;
    SourceImage::decode(&bytes, &DecodeLimits::default())
}

/// The encoded bytes of an image a request names, by `sha256:` digest of an image the session
/// holds — checked against the digest — or by artifact name.
pub(crate) async fn load_bytes(ctx: &dyn InvocationContext, image: &str) -> Result<Vec<u8>> {
    let artifacts = ctx.artifacts().ok_or_else(|| {
        VisionError::InvalidInput(format!("`{image}` cannot be loaded: the run has no artifacts"))
    })?;
    let Some(hex) = image.strip_prefix("sha256:") else {
        let part = artifacts.load(image).await.map_err(|err| {
            VisionError::InvalidInput(format!("artifact `{image}` could not be loaded: {err}"))
        })?;
        return inline_bytes(part, image);
    };
    let expected = Digest::from_hex(hex)?;
    for extension in ["png", "jpeg", "webp", "gif"] {
        if let Ok(part) = artifacts.load(&format!("vision-{hex}.{extension}")).await {
            let bytes = inline_bytes(part, image)?;
            let actual = Digest::of(&bytes);
            if actual != expected {
                return Err(VisionError::DigestMismatch { path: image.into(), expected, actual });
            }
            return Ok(bytes);
        }
    }
    Err(VisionError::InvalidInput(format!("this session holds no image {image}")))
}

fn inline_bytes(part: Part, name: &str) -> Result<Vec<u8>> {
    match part {
        Part::InlineData { data, .. } => Ok(data),
        _ => Err(VisionError::InvalidInput(format!("`{name}` does not hold an image"))),
    }
}

/// The final text event of a role's agent form.
pub(crate) fn text_event(ctx: &dyn InvocationContext, author: &str, text: String) -> Event {
    let mut event = Event::new(ctx.invocation_id());
    event.author = author.to_string();
    event.llm_response.content = Some(Content::new("model").with_text(text));
    event.llm_response.turn_complete = true;
    event
}

/// The first JSON object or array in `text`, as models wrap JSON in prose or code fences.
pub(crate) fn extract_json(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed)
        && (value.is_object() || value.is_array())
    {
        return Some(value);
    }
    let start = trimmed.find(['{', '['])?;
    let close = if trimmed[start..].starts_with('{') { '}' } else { ']' };
    let end = trimmed.rfind(close)?;
    (end > start).then(|| serde_json::from_str(&trimmed[start..=end]).ok()).flatten()
}

/// An opinion from a model's JSON answer, with its verdict one of `verdicts`.
pub(crate) fn parse_opinion(
    backend: &str,
    text: &str,
    verdicts: &[String],
    member: &str,
    provider: &str,
) -> Result<Opinion> {
    let parse = |message: String| VisionError::Parse { backend: backend.to_string(), message };
    let value = extract_json(text).ok_or_else(|| parse(format!("the answer holds no JSON: {text}")))?;
    let verdict = value
        .get("verdict")
        .and_then(Value::as_str)
        .ok_or_else(|| parse(format!("the answer names no verdict: {text}")))?;
    let normalized = normalize_verdict(verdict);
    let verdict = verdicts
        .iter()
        .find(|allowed| normalize_verdict(allowed) == normalized)
        .ok_or_else(|| {
            parse(format!("the verdict `{verdict}` is not one of {}", verdicts.join(", ")))
        })?;
    let confidence = value.get("confidence").and_then(Value::as_f64).unwrap_or(0.5) as f32;
    let mut opinion = Opinion::new(verdict.clone(), confidence);
    opinion.label = value.get("label").and_then(Value::as_str).map(str::to_string);
    opinion.rationale =
        value.get("rationale").and_then(Value::as_str).unwrap_or_default().to_string();
    opinion.member = member.to_string();
    opinion.provider = provider.to_string();
    Ok(opinion)
}

/// Collects a model's text answer.
pub(crate) async fn answer_text(
    cx: &VisionCx,
    model: &Arc<dyn adk_core::Llm>,
    request: adk_core::LlmRequest,
) -> Result<String> {
    use futures::StreamExt;
    cx.check()?;
    let mut stream = cx.scope().generate(model.as_ref(), request, false).await?;
    let mut text = String::new();
    let mut partial = String::new();
    while let Some(response) = stream.next().await {
        let response = response?;
        let target = if response.partial { &mut partial } else { &mut text };
        for part in response.content.iter().flat_map(|content| content.parts.iter()) {
            if let Part::Text { text } = part {
                target.push_str(text);
            }
        }
    }
    Ok(if text.is_empty() { partial } else { text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_json_from_prose_and_fences() {
        assert_eq!(
            extract_json("```json\n{\"verdict\": \"defect\"}\n```"),
            Some(serde_json::json!({ "verdict": "defect" }))
        );
        assert_eq!(extract_json("no json here"), None);
    }

    #[test]
    fn an_opinion_outside_the_verdicts_is_refused() {
        let verdicts = vec!["defect".to_string(), "no_defect".to_string()];
        let opinion =
            parse_opinion("m", r#"{"verdict": "No defect", "confidence": 0.9}"#, &verdicts, "r", "p")
                .unwrap();
        assert_eq!(opinion.verdict, "no_defect");
        assert!(parse_opinion("m", r#"{"verdict": "maybe"}"#, &verdicts, "r", "p").is_err());
    }
}
