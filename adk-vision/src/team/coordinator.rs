//! [`VisionTeam`]: perceive, reason, verify, and escalate a disagreement on one card.

use std::sync::Arc;

use adk_core::egress::ToolEgress;
use adk_core::{
    AdkError, Agent, Content, Event, EventStream, Evidence, GovernedCall, ImageEvidence,
    InvocationContext, ToolAuthorization, ToolConfirmationRequest, ToolEffect, ToolGate,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::roles::{Perceiver, ReasonRequest, Reasoner, Verifier, VerifyRequest};
use super::{Opinion, TeamRoleError, cx_for, request_of};
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::egress::{EgressAction, EgressReceipt, StagedEgress};
use crate::error::{Result, VisionError};
use crate::geometry::{BBox, Region, RegionSet};
use crate::media::{DecodeLimits, ImageRef, MediaScope, MediaStore, Provenance, SourceImage};
use crate::traits::DetectQuery;

/// The governed call a disagreement between reasoner and verifier is held as: its arguments
/// carry both opinions, its evidence the frame and the crop the reasoner saw. Approving it
/// accepts the reasoner's verdict; denying it keeps the verifier's.
///
/// # Example
///
/// ```
/// assert_eq!(adk_vision::team::DISAGREEMENT_TOOL, "vision_team_disagreement");
/// ```
pub const DISAGREEMENT_TOOL: &str = "vision_team_disagreement";

/// The governed call a team crop is held as when its egress rule asks a person: its evidence
/// is the exact crop that would leave, after redaction.
///
/// # Example
///
/// ```
/// assert_eq!(adk_vision::team::RELEASE_TOOL, "vision_team_release");
/// ```
pub const RELEASE_TOOL: &str = "vision_team_release";

/// The session-state key prefix the team keeps a frame's opinions under while a card waits,
/// so the resumed run asks the person about the same call.
const STATE_PREFIX: &str = "vision_team";

/// How a finding was decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum FindingSource {
    /// The perceiver was confident enough on its own.
    Perceiver,
    /// The reasoner and the verifier agreed.
    Agreed,
    /// They disagreed and a person decided.
    Person,
    /// A crop could not be released, so nobody reasoned about the region.
    Withheld,
}

/// What the team concluded about one region.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Finding {
    /// The region, in source pixels.
    pub region: Region,
    /// The verdict.
    pub verdict: String,
    /// How it was decided.
    pub source: FindingSource,
    /// The reasoner's opinion, when it was asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoner: Option<Opinion>,
    /// The verifier's opinion, when it was asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier: Option<Opinion>,
    /// The egress digest of the crop the reasoner saw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<Digest>,
}

/// One inspection of a frame: its findings, and the card the run waits on, if any.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Inspection {
    /// The frame's digest.
    pub frame: Digest,
    /// The findings decided so far, in region order.
    pub findings: Vec<Finding>,
    /// The held call the run waits on: a disagreement or a crop release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<ToolConfirmationRequest>,
    /// Opinions to keep in session state while the card waits.
    #[serde(skip)]
    state: Vec<(String, Value)>,
}

/// A perceiver, a reasoner and a verifier over one image, with disagreements escalated to a
/// person on one card each (spec 3.7 §7.12, S15).
///
/// For each region the perceiver finds:
///
/// | Score | What happens |
/// |-------|--------------|
/// | At or above the confident bound | Reported as the positive verdict |
/// | In the ambiguous range | A redacted crop goes to the reasoner; the verifier re-checks its claim against the pixels |
/// | Below the ambiguous range | Ignored |
///
/// When reasoner and verifier disagree, the team holds [`DISAGREEMENT_TOOL`] on the governed
/// path with both opinions and the frame and crop as evidence, and the run ends on that one
/// card. A crop whose egress rule asks is held first as [`RELEASE_TOOL`], with the exact crop
/// that would leave. A resumed run asks the same calls, so the person's decision applies.
///
/// # Example
///
/// ```rust,ignore
/// let team = VisionTeam::builder("inspector")
///     .perceiver(perceiver)
///     .reasoner(reasoner)
///     .verifier(verifier)
///     .question("Is this printed part defective?")
///     .verdicts("defect", "no_defect")
///     .classes(["scratch", "crack"])
///     .media_store(store)
///     .build()?;
/// ```
pub struct VisionTeam {
    name: String,
    description: String,
    perceiver: Arc<Perceiver>,
    reasoner: Arc<Reasoner>,
    verifier: Arc<Verifier>,
    question: String,
    positive: String,
    negative: String,
    classes: Vec<String>,
    ambiguous: (f32, f32),
    store: MediaStore,
}

impl std::fmt::Debug for VisionTeam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VisionTeam")
            .field("name", &self.name)
            .field("reasoner", &self.reasoner.provider())
            .field("verifier", &self.verifier.provider())
            .finish_non_exhaustive()
    }
}

/// Builds a [`VisionTeam`].
#[must_use]
pub struct VisionTeamBuilder {
    name: String,
    perceiver: Option<Arc<Perceiver>>,
    reasoner: Option<Arc<Reasoner>>,
    verifier: Option<Arc<Verifier>>,
    question: String,
    positive: String,
    negative: String,
    classes: Vec<String>,
    ambiguous: (f32, f32),
    store: Option<MediaStore>,
    same_provider: bool,
}

impl VisionTeam {
    /// A builder for a team named `name`.
    pub fn builder(name: impl Into<String>) -> VisionTeamBuilder {
        VisionTeamBuilder {
            name: name.into(),
            perceiver: None,
            reasoner: None,
            verifier: None,
            question: "Is what the region shows really there?".to_string(),
            positive: "yes".to_string(),
            negative: "no".to_string(),
            classes: Vec::new(),
            ambiguous: (0.25, 0.75),
            store: None,
            same_provider: false,
        }
    }

    /// Inspects one image, whose encoded bytes are `bytes`, under `ctx`'s run.
    ///
    /// # Errors
    ///
    /// The image's decode error, the roles' errors other than an egress hold, and the
    /// governed path's error (a freeze ends the inspection).
    pub async fn inspect(&self, ctx: &dyn InvocationContext, bytes: Vec<u8>) -> Result<Inspection> {
        let source = SourceImage::decode(&bytes, &DecodeLimits::default())?;
        let cx = cx_for(ctx);
        let scope = MediaScope::new(ctx.app_name(), ctx.user_id(), ctx.session_id());
        let mut frame_ref: Option<ImageRef> = None;
        let regions = self
            .perceiver
            .perceive(&cx, &source, &DetectQuery::Classes(self.classes.clone()))
            .await?;
        let mut inspection = Inspection {
            frame: source.digest(),
            findings: Vec::new(),
            held: None,
            state: Vec::new(),
        };
        let (low, high) = self.ambiguous;
        for (index, region) in regions.regions.iter().enumerate() {
            let score = region.score.unwrap_or(1.0);
            if score < low {
                continue;
            }
            let finding = |verdict: &str, source: FindingSource| Finding {
                region: region.clone(),
                verdict: verdict.to_string(),
                source,
                reasoner: None,
                verifier: None,
                crop: None,
            };
            if score >= high {
                inspection.findings.push(finding(&self.positive, FindingSource::Perceiver));
                continue;
            }
            let Some(bbox) = region.shape.bbox() else { continue };
            let key = format!("{STATE_PREFIX}:{}:{index}", source.digest().hex());
            let cached: Option<Opinions> = ctx
                .session()
                .state()
                .get(&key)
                .and_then(|value| serde_json::from_value(value).ok());
            let opinions = match cached {
                Some(opinions) => opinions,
                None => match self.opinions(ctx, &cx, &source, &regions, region, bbox).await? {
                    Asked::Opinions(opinions) => *opinions,
                    Asked::Held(request) => {
                        inspection.held = Some(request);
                        break;
                    }
                    Asked::Withheld => {
                        inspection.findings.push(finding("unresolved", FindingSource::Withheld));
                        continue;
                    }
                },
            };
            if opinions.reasoner.agrees_with(&opinions.verifier) {
                inspection.findings.push(Finding {
                    verdict: opinions.reasoner.verdict.clone(),
                    source: FindingSource::Agreed,
                    reasoner: Some(opinions.reasoner),
                    verifier: Some(opinions.verifier),
                    crop: opinions.crop.as_ref().map(|crop| crop.digest),
                    ..finding("", FindingSource::Agreed)
                });
                continue;
            }
            let frame = match &frame_ref {
                Some(frame) => frame.clone(),
                None => {
                    let (stored, _) =
                        self.store.put_image(&scope, bytes.clone(), Provenance::Upload).await?;
                    frame_ref = Some(stored.clone());
                    stored
                }
            };
            let (args, evidence) =
                self.disagreement_card(&regions, region, bbox, &frame, &opinions);
            let call_id = format!("{DISAGREEMENT_TOOL}-{}-{index}", &source.digest().hex()[..16]);
            match hold(ctx, DISAGREEMENT_TOOL, &call_id, args, evidence).await? {
                ToolAuthorization::Execute { .. } => inspection.findings.push(Finding {
                    verdict: opinions.reasoner.verdict.clone(),
                    source: FindingSource::Person,
                    reasoner: Some(opinions.reasoner),
                    verifier: Some(opinions.verifier),
                    crop: opinions.crop.as_ref().map(|crop| crop.digest),
                    ..finding("", FindingSource::Person)
                }),
                ToolAuthorization::Refuse { .. } => inspection.findings.push(Finding {
                    verdict: opinions.verifier.verdict.clone(),
                    source: FindingSource::Person,
                    reasoner: Some(opinions.reasoner),
                    verifier: Some(opinions.verifier),
                    crop: opinions.crop.as_ref().map(|crop| crop.digest),
                    ..finding("", FindingSource::Person)
                }),
                ToolAuthorization::Pending { request, .. } => {
                    tracing::info!(
                        team.name = %self.name,
                        vision.frame = %source.digest(),
                        "reasoner and verifier disagree; held for a person"
                    );
                    inspection
                        .state
                        .push((key, serde_json::to_value(&opinions).unwrap_or_default()));
                    inspection.held = Some(request);
                    break;
                }
            }
        }
        Ok(inspection)
    }

    /// Asks the reasoner and the verifier about one region, holding a crop whose rule asks.
    async fn opinions(
        &self,
        ctx: &dyn InvocationContext,
        cx: &VisionCx,
        source: &SourceImage,
        regions: &RegionSet,
        region: &Region,
        bbox: BBox,
    ) -> Result<Asked> {
        let reasoner_approval = match self.reasoner.stage(cx, source, bbox).await {
            Ok(staged) => match release_approval(ctx, self.reasoner.name(), staged).await? {
                Release::Approved(digest) => digest,
                Release::Held(request) => return Ok(Asked::Held(request)),
                Release::Refused => return Ok(Asked::Withheld),
            },
            Err(VisionError::EgressRefused { .. }) => return Ok(Asked::Withheld),
            Err(err) => return Err(err),
        };
        let mut request = ReasonRequest::new(
            self.question.clone(),
            [self.positive.clone(), self.negative.clone()],
            regions.clone(),
        )
        .with_crop(source, bbox);
        if let Some(digest) = reasoner_approval {
            request = request.approving(digest);
        }
        let reasoned = self.reasoner.reason(cx, request).await?;
        let mut claim = reasoned.opinion.clone();
        claim.regions = Some(RegionSet::new(
            regions.image,
            regions.width,
            regions.height,
            vec![region.clone()],
        ));
        let verifier_approval = match self.verifier.stage(cx, source, bbox).await {
            Ok(staged) => match release_approval(ctx, self.verifier.name(), staged).await? {
                Release::Approved(digest) => digest,
                Release::Held(request) => return Ok(Asked::Held(request)),
                Release::Refused => return Ok(Asked::Withheld),
            },
            Err(VisionError::EgressRefused { .. }) => return Ok(Asked::Withheld),
            Err(err) => return Err(err),
        };
        let label = region.label.clone().or_else(|| claim.label.clone());
        let mut verify = VerifyRequest::new(
            self.question.clone(),
            self.positive.clone(),
            self.negative.clone(),
            claim,
            bbox,
            source,
        );
        if let Some(label) = label {
            verify = verify.with_label(label);
        }
        if let Some(digest) = verifier_approval {
            verify.approved.push(digest);
        }
        let verified = self.verifier.verify(cx, verify).await?;
        let mut verifier = verified.opinion;
        verifier.regions = Some(RegionSet::new(
            regions.image,
            regions.width,
            regions.height,
            verified.check.found.clone(),
        ));
        Ok(Asked::Opinions(Box::new(Opinions {
            reasoner: reasoned.opinion,
            verifier,
            crop: reasoned.released.first().map(CropRecord::from),
        })))
    }

    /// The arguments and evidence of a disagreement card.
    fn disagreement_card(
        &self,
        regions: &RegionSet,
        region: &Region,
        bbox: BBox,
        frame: &ImageRef,
        opinions: &Opinions,
    ) -> (Value, Vec<Evidence>) {
        let args = json!({
            "question": self.question,
            "frame": frame.digest,
            "region": {
                "label": region.label,
                "score": region.score,
                "box": [bbox.x0, bbox.y0, bbox.x1, bbox.y1],
            },
            "reasoner": opinions.reasoner,
            "verifier": opinions.verifier,
            "crop": opinions.crop.as_ref().map(|crop| crop.digest),
            "onApprove": format!("accept the reasoner's verdict `{}`", opinions.reasoner.verdict),
            "onDeny": format!("keep the verifier's verdict `{}`", opinions.verifier.verdict),
        });
        let mut overlay = vec![labelled(
            region,
            format!(
                "reasoner: {} ({:.2})",
                opinions.reasoner.verdict, opinions.reasoner.confidence
            ),
        )];
        if let Some(found) = &opinions.verifier.regions {
            overlay.extend(found.regions.iter().map(|r| labelled(r, "verifier found".to_string())));
        }
        let overlay = RegionSet::new(regions.image, regions.width, regions.height, overlay);
        let mut evidence = vec![Evidence::from(
            ImageEvidence::new(
                frame.digest.to_string(),
                frame.artifact.clone(),
                frame.mime_type.clone(),
                frame.width,
                frame.height,
            )
            .with_regions(serde_json::to_value(&overlay).unwrap_or_default())
            .with_caption(format!(
                "The frame: the reasoner says `{}`, the verifier says `{}`",
                opinions.reasoner.verdict, opinions.verifier.verdict
            )),
        )];
        if let Some(crop) = &opinions.crop
            && let Some(artifact) = &crop.artifact
        {
            evidence.push(Evidence::from(
                ImageEvidence::new(
                    crop.digest.to_string(),
                    artifact.clone(),
                    "image/png",
                    crop.width,
                    crop.height,
                )
                .with_caption("The crop the reasoner saw, after redaction"),
            ));
        }
        (args, evidence)
    }
}

/// A region relabelled for the card's overlay.
fn labelled(region: &Region, label: String) -> Region {
    let mut region = region.clone();
    region.label = Some(label);
    region
}

/// The opinions about one region, kept in session state while their card waits.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Opinions {
    reasoner: Opinion,
    verifier: Opinion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    crop: Option<CropRecord>,
}

/// The crop the reasoner was sent: its egress digest, artifact and size.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CropRecord {
    digest: Digest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    artifact: Option<adk_core::ArtifactLocator>,
    width: u32,
    height: u32,
}

impl From<&EgressReceipt> for CropRecord {
    fn from(receipt: &EgressReceipt) -> Self {
        let (width, height) = receipt.chain.output_size();
        Self { digest: receipt.egress, artifact: receipt.artifact.clone(), width, height }
    }
}

enum Asked {
    Opinions(Box<Opinions>),
    Held(ToolConfirmationRequest),
    Withheld,
}

enum Release {
    /// Released without asking (`None`), or with this approved digest.
    Approved(Option<Digest>),
    Held(ToolConfirmationRequest),
    Refused,
}

/// Holds a staged crop for a person when its rule asks.
async fn release_approval(
    ctx: &dyn InvocationContext,
    member: &str,
    staged: Option<StagedEgress>,
) -> Result<Release> {
    let Some(staged) = staged else { return Ok(Release::Approved(None)) };
    match staged.action() {
        EgressAction::Allow => Ok(Release::Approved(None)),
        EgressAction::Deny => Ok(Release::Refused),
        EgressAction::Ask => {
            let digest = staged.digest();
            let args = json!({
                "member": member,
                "egress": digest,
                "hosts": staged.hosts(),
            });
            let evidence = staged.image_evidence().map(Evidence::from).into_iter().collect();
            let call_id = format!("{RELEASE_TOOL}-{member}-{}", &digest.hex()[..16]);
            Ok(match hold(ctx, RELEASE_TOOL, &call_id, args, evidence).await? {
                ToolAuthorization::Execute { .. } => Release::Approved(Some(digest)),
                ToolAuthorization::Refuse { .. } => Release::Refused,
                ToolAuthorization::Pending { request, .. } => Release::Held(request),
            })
        }
    }
}

/// Puts one call that needs a person's decision through the governed path.
async fn hold(
    ctx: &dyn InvocationContext,
    tool: &str,
    call_id: &str,
    args: Value,
    evidence: Vec<Evidence>,
) -> Result<ToolAuthorization> {
    let gate = ToolGate::for_context(ctx);
    let mut call = GovernedCall::new(tool, call_id, args)
        .with_effect(ToolEffect::Idempotent)
        .with_egress(ToolEgress::None)
        .with_evidence(evidence);
    call.requires_confirmation = true;
    adk_core::authorize_tool_call(&gate, call).await.map_err(VisionError::from)
}

impl VisionTeamBuilder {
    /// The perceiver.
    pub fn perceiver(mut self, perceiver: impl Into<Arc<Perceiver>>) -> Self {
        self.perceiver = Some(perceiver.into());
        self
    }

    /// The reasoner.
    pub fn reasoner(mut self, reasoner: impl Into<Arc<Reasoner>>) -> Self {
        self.reasoner = Some(reasoner.into());
        self
    }

    /// The verifier.
    pub fn verifier(mut self, verifier: impl Into<Arc<Verifier>>) -> Self {
        self.verifier = Some(verifier.into());
        self
    }

    /// The question asked about each ambiguous region.
    pub fn question(mut self, question: impl Into<String>) -> Self {
        self.question = question.into();
        self
    }

    /// The two verdicts: `positive` says what the region shows is there, `negative` that it
    /// is not.
    pub fn verdicts(mut self, positive: impl Into<String>, negative: impl Into<String>) -> Self {
        self.positive = positive.into();
        self.negative = negative.into();
        self
    }

    /// The classes the perceiver looks for.
    pub fn classes(mut self, classes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.classes = classes.into_iter().map(Into::into).collect();
        self
    }

    /// The ambiguous score range `[low, high)`: regions in it go to the reasoner, regions at
    /// or above `high` are reported as they are, regions below `low` are ignored. `[0.25,
    /// 0.75)` by default.
    pub fn ambiguous(mut self, low: f32, high: f32) -> Self {
        self.ambiguous = (low, high);
        self
    }

    /// Where a frame with a card is stored, so the card can show it.
    pub fn media_store(mut self, store: MediaStore) -> Self {
        self.store = Some(store);
        self
    }

    /// Lets the verifier run on the reasoner's provider.
    pub fn allow_same_provider(mut self) -> Self {
        self.same_provider = true;
        self
    }

    /// Builds the team.
    ///
    /// # Errors
    ///
    /// [`TeamRoleError::Missing`] for a missing role or media store, and
    /// [`TeamRoleError::SameProvider`] when the verifier runs on the reasoner's provider and
    /// [`allow_same_provider`](Self::allow_same_provider) was not called.
    pub fn build(self) -> std::result::Result<VisionTeam, TeamRoleError> {
        let missing =
            |what: &str| TeamRoleError::Missing(format!("the team '{}' needs {what}", self.name));
        let perceiver = self.perceiver.clone().ok_or_else(|| missing("a perceiver"))?;
        let reasoner = self.reasoner.clone().ok_or_else(|| missing("a reasoner"))?;
        let verifier = self.verifier.clone().ok_or_else(|| missing("a verifier"))?;
        let store = self.store.clone().ok_or_else(|| missing("a media store for its cards"))?;
        if !self.same_provider && reasoner.provider() == verifier.provider() {
            return Err(TeamRoleError::SameProvider { provider: reasoner.provider().to_string() });
        }
        Ok(VisionTeam {
            description: format!(
                "Inspects an image with a perceiver, a reasoner ({}) and a verifier ({}), and asks \
                 a person when they disagree.",
                reasoner.provider(),
                verifier.provider()
            ),
            name: self.name,
            perceiver,
            reasoner,
            verifier,
            question: self.question,
            positive: self.positive,
            negative: self.negative,
            classes: self.classes,
            ambiguous: self.ambiguous,
            store,
        })
    }
}

#[async_trait]
impl Agent for VisionTeam {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        let request = request_of(ctx.as_ref());
        let image = request.get("image").and_then(Value::as_str).unwrap_or_default().to_string();
        let bytes = super::load_bytes(ctx.as_ref(), &image).await?;
        let inspection = self.inspect(ctx.as_ref(), bytes).await?;
        let mut event = Event::new(ctx.invocation_id());
        event.author = self.name.clone();
        event.llm_response.turn_complete = true;
        for (key, value) in &inspection.state {
            event.actions.state_delta.insert(key.clone(), value.clone());
        }
        match &inspection.held {
            Some(request) => {
                event.llm_response.interrupted = true;
                event.llm_response.content = Some(Content::new("model").with_text(format!(
                    "A person's decision is needed for '{}'. Provide approve/deny decision to \
                     continue.",
                    request.tool_name
                )));
                event.actions.tool_confirmation = Some(request.clone());
            }
            None => {
                let text = serde_json::to_string(&inspection).map_err(|err| {
                    AdkError::agent(format!("the inspection could not be encoded: {err}"))
                })?;
                event.llm_response.content = Some(Content::new("model").with_text(text));
            }
        }
        Ok(Box::pin(futures::stream::iter([Ok(event)])))
    }
}
