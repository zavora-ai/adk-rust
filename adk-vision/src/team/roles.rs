//! The perceiver, reasoner and verifier roles.

use std::sync::Arc;

use adk_core::egress::ToolEgress;
use adk_core::{
    AdkError, Agent, Content, ErrorCategory, ErrorComponent, EventStream, InvocationContext, Llm,
    LlmRequest, Part, Tool, ToolContext, ToolEffect,
};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::{
    Opinion, TeamRoleError, answer_text, cx_for, load_image, parse_opinion, provider_of,
    request_of, text_event,
};
use crate::backend::{BackendDescriptor, Locality};
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::egress::{
    DataClass, EgressGate, EgressReceipt, ReleaseRequest, ReleasedImage, StagedEgress,
};
use crate::error::{Result, VisionError};
use crate::geometry::{BBox, CoordFrame, Region, RegionSet};
use crate::manifest::VisionTask;
use crate::media::SourceImage;
use crate::runtime::VisionRuntime;
use crate::toolset::{VISION_TOOL_NAMES, VisionToolset};
use crate::traits::DetectQuery;

const ALL_TASKS: [VisionTask; 7] = [
    VisionTask::Detect,
    VisionTask::Segment,
    VisionTask::Read,
    VisionTask::Embed,
    VisionTask::Describe,
    VisionTask::AnalyzeVideo,
    VisionTask::Edit,
];

/// The margin, in pixels, around a region when its crop is sent to a reasoner or verifier.
const CROP_MARGIN: f32 = 16.0;

/// Refuses a runtime that has a remote backend.
fn check_local_runtime(
    role: &'static str,
    member: &str,
    runtime: &VisionRuntime,
) -> std::result::Result<(), TeamRoleError> {
    for id in runtime.backends() {
        for task in ALL_TASKS {
            if let Some(descriptor) = runtime.descriptor(task, &id)
                && let Locality::Remote { hosts } = &descriptor.locality
            {
                return Err(TeamRoleError::RemoteBackend {
                    role,
                    member: member.to_string(),
                    backend: id.to_string(),
                    hosts: hosts.join(", "),
                });
            }
        }
    }
    Ok(())
}

/// Refuses a tool that declares any network use, or a vision tool given without its runtime.
fn check_local_tool(
    role: &'static str,
    member: &str,
    tool: &dyn Tool,
) -> std::result::Result<(), TeamRoleError> {
    if VISION_TOOL_NAMES.contains(&tool.name()) {
        return Err(TeamRoleError::UncheckedVisionTool {
            role,
            member: member.to_string(),
            tool: tool.name().to_string(),
        });
    }
    match tool.egress(&json!({})) {
        ToolEgress::None => Ok(()),
        other => Err(TeamRoleError::RemoteTool {
            role,
            member: member.to_string(),
            tool: tool.name().to_string(),
            egress: describe_egress(&other),
        }),
    }
}

fn describe_egress(egress: &ToolEgress) -> String {
    match egress {
        ToolEgress::None => "no network use".to_string(),
        ToolEgress::Undeclared => "nothing about its network use".to_string(),
        ToolEgress::Configured => "endpoints fixed when it was built".to_string(),
        ToolEgress::Enforced => "connections checked by the run's egress policy".to_string(),
        ToolEgress::Destinations(rules) => format!("destinations {rules:?}"),
    }
}

/// A tool that runs only calls that declare no network use (spec 3.7 §7.12).
///
/// A perceiver's and a verifier's tools are wrapped in it: a call whose declared egress is
/// anything but [`ToolEgress::None`] — a vision call routed to a remote backend, say — is
/// refused before the tool runs, so the role holds no tool that can send data off the
/// machine even when one of its tools could route a call remotely.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_core::{Tool, ToolContext};
/// use adk_vision::team::LocalOnlyTool;
/// use serde_json::Value;
///
/// struct Count;
///
/// #[async_trait::async_trait]
/// impl Tool for Count {
///     fn name(&self) -> &str { "count" }
///     fn description(&self) -> &str { "Counts regions" }
///     async fn execute(&self, _: Arc<dyn ToolContext>, _: Value) -> adk_core::Result<Value> {
///         Ok(Value::Null)
///     }
/// }
///
/// let guarded = LocalOnlyTool::new(Arc::new(Count), "perceiver");
/// assert_eq!(guarded.name(), "count");
/// ```
pub struct LocalOnlyTool {
    inner: Arc<dyn Tool>,
    member: String,
}

impl LocalOnlyTool {
    /// Wraps `inner` for the member named `member`.
    pub fn new(inner: Arc<dyn Tool>, member: impl Into<String>) -> Self {
        Self { inner, member: member.into() }
    }
}

#[async_trait]
impl Tool for LocalOnlyTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn declaration(&self) -> Value {
        self.inner.declaration()
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }

    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }

    fn origin(&self) -> &str {
        self.inner.origin()
    }

    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }

    fn effect(&self) -> ToolEffect {
        self.inner.effect()
    }

    fn is_concurrency_safe(&self) -> bool {
        self.inner.is_concurrency_safe()
    }

    fn egress(&self, args: &Value) -> ToolEgress {
        self.inner.egress(args)
    }

    fn approval_evidence(&self, args: &Value) -> Vec<adk_core::Evidence> {
        self.inner.approval_evidence(args)
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        match self.inner.egress(&args) {
            ToolEgress::None => self.inner.execute(ctx, args).await,
            other => Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Forbidden,
                "vision.team.remote_call_refused",
                format!(
                    "`{}` was not run: this call declares {}, and the member '{}' holds only \
                     tools that stay on the device",
                    self.inner.name(),
                    describe_egress(&other),
                    self.member
                ),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Perceiver
// ---------------------------------------------------------------------------

/// The perceiver: on-device perception with a local model or none (spec 3.7 §7.12).
///
/// Built only through [`Perceiver::builder`], which refuses a remote model, a runtime with a
/// remote backend and any tool that declares network use. With a model it runs as an
/// `LlmAgent` over its tools, each a [`LocalOnlyTool`]; without one, its agent form detects
/// in the image its request names and answers the `RegionSet`.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_vision::team::Perceiver;
/// use adk_vision::{
///     BBox, BackendDescriptor, CoordFrame, DetectQuery, Detector, Locality, RawRegion,
///     SentImage, VisionCx, VisionRuntime,
/// };
///
/// struct Marks(BackendDescriptor);
///
/// #[async_trait::async_trait]
/// impl Detector for Marks {
///     fn descriptor(&self) -> &BackendDescriptor { &self.0 }
///     async fn detect(&self, _: &VisionCx, _: &SentImage, _: &DetectQuery) -> adk_vision::Result<Vec<RawRegion>> {
///         Ok(vec![RawRegion::boxed(BBox::new(4.0, 4.0, 12.0, 12.0), "scratch", 0.4)])
///     }
/// }
///
/// let local = BackendDescriptor::new("marks", Locality::InProcess, CoordFrame::SentPixels);
/// let runtime = VisionRuntime::builder().detector(Arc::new(Marks(local))).build().unwrap();
/// let perceiver = Perceiver::builder("perceiver").runtime(Arc::new(runtime)).build().unwrap();
/// assert_eq!(perceiver.name(), "perceiver");
/// ```
pub struct Perceiver {
    name: String,
    description: String,
    runtime: Option<Arc<VisionRuntime>>,
    tools: Vec<Arc<dyn Tool>>,
    classes: Vec<String>,
    agent: Option<Arc<dyn Agent>>,
}

impl std::fmt::Debug for Perceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Perceiver")
            .field("name", &self.name)
            .field("tools", &self.tools.iter().map(|tool| tool.name()).collect::<Vec<_>>())
            .field("model", &self.agent.is_some())
            .finish_non_exhaustive()
    }
}

/// Builds a [`Perceiver`].
#[must_use]
pub struct PerceiverBuilder {
    name: String,
    description: String,
    runtime: Option<Arc<VisionRuntime>>,
    toolset: Option<VisionToolset>,
    model: Option<Arc<dyn Llm>>,
    tools: Vec<Arc<dyn Tool>>,
    classes: Vec<String>,
    instruction: Option<String>,
}

impl Perceiver {
    /// A builder for a perceiver named `name`.
    pub fn builder(name: impl Into<String>) -> PerceiverBuilder {
        PerceiverBuilder {
            name: name.into(),
            description: "Perceives on the device and reports regions in source pixels."
                .to_string(),
            runtime: None,
            toolset: None,
            model: None,
            tools: Vec::new(),
            classes: Vec::new(),
            instruction: None,
        }
    }

    /// The member name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The tools the perceiver holds, each a [`LocalOnlyTool`].
    pub fn tools(&self) -> &[Arc<dyn Tool>] {
        &self.tools
    }

    /// Detects `query` in `image` on the device.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for a perceiver built without a runtime, and the
    /// runtime's error.
    pub async fn perceive(
        &self,
        cx: &VisionCx,
        image: &SourceImage,
        query: &DetectQuery,
    ) -> Result<RegionSet> {
        let runtime = self.runtime.as_ref().ok_or_else(|| {
            VisionError::InvalidInput(format!(
                "the perceiver '{}' has no runtime to detect with; give it one with \
                 PerceiverBuilder::runtime",
                self.name
            ))
        })?;
        let found = runtime.detect(cx, image, query).await?;
        let (width, height) = image.size();
        Ok(RegionSet::new(image.digest(), width, height, found.value))
    }

    /// The classes the agent form detects when a request names none.
    pub fn classes(&self) -> &[String] {
        &self.classes
    }
}

impl PerceiverBuilder {
    /// The model-facing description.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// The runtime [`Perceiver::perceive`] detects with; every backend must be on the device.
    pub fn runtime(mut self, runtime: Arc<VisionRuntime>) -> Self {
        self.runtime = Some(runtime);
        self
    }

    /// The vision tools; their runtime must have only on-device backends. Also the runtime
    /// [`Perceiver::perceive`] uses when none is given.
    pub fn toolset(mut self, toolset: VisionToolset) -> Self {
        self.toolset = Some(toolset);
        self
    }

    /// A model that runs on the device; without one the perceiver runs no model.
    pub fn model(mut self, model: Arc<dyn Llm>) -> Self {
        self.model = Some(model);
        self
    }

    /// Another tool; it must declare no network use ([`ToolEgress::None`]).
    pub fn tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// The classes the agent form detects when a request names none.
    pub fn classes(mut self, classes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.classes = classes.into_iter().map(Into::into).collect();
        self
    }

    /// The model's instruction.
    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    /// Builds the perceiver.
    ///
    /// # Errors
    ///
    /// [`TeamRoleError::RemoteModel`] for a model whose requests leave the machine,
    /// [`TeamRoleError::RemoteBackend`] for a runtime with a remote backend,
    /// [`TeamRoleError::RemoteTool`] for a tool that declares any network use,
    /// [`TeamRoleError::UncheckedVisionTool`] for a vision tool given through
    /// [`tool`](Self::tool), and [`TeamRoleError::Missing`] for a perceiver with neither a
    /// runtime nor a model.
    pub fn build(self) -> std::result::Result<Perceiver, TeamRoleError> {
        const ROLE: &str = "perceiver";
        if let Some(model) = &self.model
            && model.data_locality().leaves_machine()
        {
            return Err(TeamRoleError::RemoteModel {
                role: ROLE,
                member: self.name,
                model: model.name().to_string(),
            });
        }
        let runtime =
            self.runtime.or_else(|| self.toolset.as_ref().map(|set| Arc::clone(set.runtime())));
        if let Some(runtime) = &runtime {
            check_local_runtime(ROLE, &self.name, runtime)?;
        }
        if let Some(toolset) = &self.toolset {
            check_local_runtime(ROLE, &self.name, toolset.runtime())?;
        }
        for tool in &self.tools {
            check_local_tool(ROLE, &self.name, tool.as_ref())?;
        }
        if runtime.is_none() && self.model.is_none() {
            return Err(TeamRoleError::Missing(format!(
                "the perceiver '{}' needs a runtime to detect with or a local model to drive its \
                 tools",
                self.name
            )));
        }
        let tools: Vec<Arc<dyn Tool>> = self
            .toolset
            .iter()
            .flat_map(VisionToolset::tools)
            .chain(self.tools)
            .map(|tool| Arc::new(LocalOnlyTool::new(tool, self.name.clone())) as Arc<dyn Tool>)
            .collect();
        let agent = match self.model {
            Some(model) => {
                let instruction = self.instruction.unwrap_or_else(|| {
                    "You are the perceiver of a vision team. Look at the image you are given \
                     with your tools, which all run on this device, and report what you found \
                     as regions in source pixels, with labels and scores."
                        .to_string()
                });
                let mut builder = adk_agent::LlmAgentBuilder::new(self.name.clone())
                    .description(self.description.clone())
                    .model(model)
                    .instruction(instruction);
                for tool in &tools {
                    builder = builder.tool(Arc::clone(tool));
                }
                let agent = builder
                    .build()
                    .map_err(|err| TeamRoleError::Missing(format!("perceiver agent: {err}")))?;
                Some(Arc::new(agent) as Arc<dyn Agent>)
            }
            None => None,
        };
        Ok(Perceiver {
            name: self.name,
            description: self.description,
            runtime,
            tools,
            classes: self.classes,
            agent,
        })
    }
}

#[async_trait]
impl Agent for Perceiver {
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
        if let Some(agent) = &self.agent {
            return agent.run(ctx).await;
        }
        let request = request_of(ctx.as_ref());
        let image = request.get("image").and_then(Value::as_str).unwrap_or_default().to_string();
        let classes: Vec<String> = match request.get("classes").and_then(Value::as_array) {
            Some(items) => items.iter().filter_map(Value::as_str).map(str::to_string).collect(),
            None => self.classes.clone(),
        };
        let cx = cx_for(ctx.as_ref());
        let source = load_image(ctx.as_ref(), &image).await?;
        let regions = self.perceive(&cx, &source, &DetectQuery::Classes(classes)).await?;
        let text = serde_json::to_string(&regions)
            .map_err(|err| AdkError::agent(format!("regions could not be encoded: {err}")))?;
        let event = text_event(ctx.as_ref(), &self.name, text);
        Ok(Box::pin(futures::stream::iter([Ok(event)])))
    }
}

// ---------------------------------------------------------------------------
// Reasoner
// ---------------------------------------------------------------------------

/// What the coordinator asks a reasoner: a question, the perceiver's regions, measurements,
/// and the regions of the source image to send as crops.
///
/// The crops are released through the reasoner's egress gate — cropped, redacted, decided
/// and recorded — before the model sees them; a reasoner never receives the source image.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ReasonRequest<'a> {
    /// The question.
    pub question: String,
    /// The verdicts the answer must be one of.
    pub verdicts: Vec<String>,
    /// The perceiver's regions, in source pixels.
    pub regions: RegionSet,
    /// Measurements, such as `vision_measure` results.
    pub measurements: Vec<Value>,
    /// The source image the crops are cut from.
    pub source: Option<&'a SourceImage>,
    /// The regions to crop, in source pixels; a margin is added.
    pub crops: Vec<BBox>,
    /// Egress digests a person approved, for a rule that asks.
    pub approved: Vec<Digest>,
}

impl<'a> ReasonRequest<'a> {
    /// A request asking `question` over `regions`, answered with one of `verdicts`.
    pub fn new(
        question: impl Into<String>,
        verdicts: impl IntoIterator<Item = impl Into<String>>,
        regions: RegionSet,
    ) -> Self {
        Self {
            question: question.into(),
            verdicts: verdicts.into_iter().map(Into::into).collect(),
            regions,
            measurements: Vec::new(),
            source: None,
            crops: Vec::new(),
            approved: Vec::new(),
        }
    }

    /// Sends `crop` of `source`, after the egress gate.
    #[must_use]
    pub fn with_crop(mut self, source: &'a SourceImage, crop: BBox) -> Self {
        self.source = Some(source);
        self.crops.push(crop);
        self
    }

    /// Adds measurements.
    #[must_use]
    pub fn with_measurements(mut self, measurements: Vec<Value>) -> Self {
        self.measurements = measurements;
        self
    }

    /// Releases a crop whose egress digest a person approved.
    #[must_use]
    pub fn approving(mut self, digest: Digest) -> Self {
        self.approved.push(digest);
        self
    }
}

/// A reasoner's answer and the receipts of the crops it was sent.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Reasoned {
    /// The opinion.
    pub opinion: Opinion,
    /// One receipt per released crop.
    pub released: Vec<EgressReceipt>,
}

/// The reasoner: a model, usually in the cloud, that answers over regions, measurements and
/// crops the egress gate released, and holds no tool (spec 3.7 §7.12).
///
/// Every image a request carries goes through the gate first; the request is then screened by
/// an `ImagePrivacyGuardrail` on the same gate, so an image that did not pass the gate never
/// reaches a remote model.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_core::egress::EgressPolicy;
/// use adk_vision::team::Reasoner;
/// use adk_vision::{EgressConfig, EgressGate, InMemoryEgressRecorder};
///
/// # struct Cloud;
/// # #[async_trait::async_trait]
/// # impl adk_core::Llm for Cloud {
/// #     fn name(&self) -> &str { "claude-haiku-5-5" }
/// #     async fn generate_content(&self, _: adk_core::LlmRequest, _: bool) -> adk_core::Result<adk_core::LlmResponseStream> {
/// #         Ok(Box::pin(futures::stream::empty()))
/// #     }
/// # }
/// let gate = Arc::new(EgressGate::new(
///     EgressPolicy::public_only(),
///     EgressConfig::default(),
///     Arc::new(InMemoryEgressRecorder::new()),
/// ));
/// let reasoner = Reasoner::builder("reasoner")
///     .model(Arc::new(Cloud))
///     .egress_gate(gate)
///     .hosts(["api.anthropic.com"])
///     .build()
///     .unwrap();
/// assert_eq!(reasoner.provider(), "anthropic");
/// ```
pub struct Reasoner {
    name: String,
    description: String,
    model: Arc<dyn Llm>,
    gate: Option<Arc<EgressGate>>,
    destination: BackendDescriptor,
    data_class: DataClass,
    provider: String,
    #[cfg(feature = "guardrail")]
    guardrail: Option<crate::guardrail::ImagePrivacyGuardrail>,
    max_output_tokens: Option<i32>,
    config: adk_core::GenerateContentConfig,
}

impl std::fmt::Debug for Reasoner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reasoner")
            .field("name", &self.name)
            .field("model", &self.model.name())
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

/// Builds a [`Reasoner`].
#[must_use]
pub struct ReasonerBuilder {
    name: String,
    description: String,
    model: Option<Arc<dyn Llm>>,
    gate: Option<Arc<EgressGate>>,
    hosts: Vec<String>,
    data_class: String,
    provider: Option<String>,
    max_output_tokens: Option<i32>,
    config: adk_core::GenerateContentConfig,
}

impl Reasoner {
    /// A builder for a reasoner named `name`.
    pub fn builder(name: impl Into<String>) -> ReasonerBuilder {
        ReasonerBuilder {
            name: name.into(),
            description: "Reasons over regions, measurements and redacted crops, and answers \
                          with a verdict."
                .to_string(),
            model: None,
            gate: None,
            hosts: Vec::new(),
            data_class: "team_crop".to_string(),
            provider: None,
            max_output_tokens: None,
            config: adk_core::GenerateContentConfig::default(),
        }
    }

    /// The member name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The provider the reasoner runs on.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// The model.
    pub fn model(&self) -> &Arc<dyn Llm> {
        &self.model
    }

    /// Whether the reasoner's requests leave the machine.
    pub fn is_remote(&self) -> bool {
        self.model.data_locality().leaves_machine()
    }

    /// The egress gate the reasoner's crops pass.
    pub fn egress_gate(&self) -> Option<&Arc<EgressGate>> {
        self.gate.as_ref()
    }

    /// The data class the reasoner's crops are released as.
    pub fn data_class(&self) -> &DataClass {
        &self.data_class
    }

    /// Stages `crop` of `source` for a person's approval when its egress rule asks: the
    /// destination, minimise and redact steps, the egress copy stored. `None` for a reasoner
    /// on the device, or under a rule that allows the release.
    ///
    /// # Errors
    ///
    /// The gate's refusals ([`EgressGate::stage`]), [`VisionError::EgressRefused`] for a
    /// rule that denies.
    pub async fn stage(
        &self,
        cx: &VisionCx,
        source: &SourceImage,
        crop: BBox,
    ) -> Result<Option<StagedEgress>> {
        let Some(gate) = self.gate.as_ref().filter(|_| self.is_remote()) else {
            return Ok(None);
        };
        if allows(gate, &self.destination, &self.data_class) {
            return Ok(None);
        }
        let request = ReleaseRequest::new(source, &self.destination, self.data_class.clone())
            .with_crop(with_margin(crop, source));
        gate.stage(cx, &request).await.map(Some)
    }

    /// Asks the reasoner.
    ///
    /// # Errors
    ///
    /// The gate's refusals for a crop ([`VisionError::EgressHeld`] under a rule that asks
    /// without an approved digest), the model's error, and [`VisionError::Parse`] for an
    /// answer that is not an opinion with one of the verdicts.
    pub async fn reason(&self, cx: &VisionCx, request: ReasonRequest<'_>) -> Result<Reasoned> {
        let mut parts = Vec::new();
        let mut released = Vec::new();
        if !request.crops.is_empty() {
            let source = request.source.ok_or_else(|| {
                VisionError::InvalidInput("crops need the source image they are cut from".into())
            })?;
            for crop in &request.crops {
                let crop = with_margin(*crop, source);
                if self.is_remote() {
                    let gate = self.gate.as_ref().ok_or_else(|| VisionError::NotReleased {
                        backend: self.destination.id.to_string(),
                    })?;
                    let mut release =
                        ReleaseRequest::new(source, &self.destination, self.data_class.clone())
                            .with_crop(crop);
                    for digest in &request.approved {
                        release = release.approved(*digest);
                    }
                    let image: ReleasedImage = gate.release(cx, release).await?;
                    parts.push(image.to_part());
                    released.push(image.receipt().clone());
                } else {
                    let (w, h) = source.size();
                    let (x, y, cw, ch) = crop.pixel_bounds(w, h);
                    let pixels = crate::edit::ops::crop(source.pixels(), x, y, cw, ch);
                    parts.push(Part::InlineData {
                        mime_type: "image/png".into(),
                        data: pixels.encode_png()?,
                        uri: None,
                        annotations: None,
                    });
                }
            }
        }
        let opinion = self
            .ask(
                cx,
                &request.question,
                &request.verdicts,
                &request.regions,
                &request.measurements,
                parts,
            )
            .await?;
        Ok(Reasoned { opinion, released })
    }

    async fn ask(
        &self,
        cx: &VisionCx,
        question: &str,
        verdicts: &[String],
        regions: &RegionSet,
        measurements: &[Value],
        images: Vec<Part>,
    ) -> Result<Opinion> {
        let count = images.len();
        let mut text = format!(
            "You are the reasoner of a vision team. You hold no tools and never see the whole \
             image: you see the regions a perceiver on the device found, measurements, and \
             {count} crop(s) of the image around those regions, redacted before they were sent.\n\n\
             Question: {question}\n\
             Answer with one verdict from: {}.\n\n\
             Regions, in source pixels: {}\n",
            verdicts.iter().map(|v| format!("\"{v}\"")).collect::<Vec<_>>().join(", "),
            serde_json::to_string(regions).unwrap_or_default(),
        );
        if !measurements.is_empty() {
            text.push_str(&format!(
                "Measurements: {}\n",
                serde_json::to_string(measurements).unwrap_or_default()
            ));
        }
        text.push_str(
            "\nAnswer with JSON only: {\"verdict\": <one of the verdicts>, \"label\": <the class \
             you judged>, \"confidence\": <0 to 1>, \"rationale\": <one sentence>}.",
        );
        let mut parts = images;
        parts.push(Part::Text { text });
        let mut content = Content { role: "user".into(), parts };
        #[cfg(feature = "guardrail")]
        if let Some(guardrail) = &self.guardrail {
            let outcome = guardrail.gate_content(cx, &mut content).await;
            if !outcome.withheld.is_empty() {
                tracing::warn!(
                    vision.withheld = outcome.withheld.len(),
                    team.member = %self.name,
                    "images withheld from the reasoner"
                );
            }
        }
        let mut request = LlmRequest::new(self.model.name(), vec![content]);
        let mut config = self.config.clone();
        if self.max_output_tokens.is_some() {
            config.max_output_tokens = self.max_output_tokens;
        }
        request.config = Some(config);
        let text = answer_text(cx, &self.model, request).await?;
        parse_opinion(self.model.name(), &text, verdicts, &self.name, &self.provider)
    }
}

impl ReasonerBuilder {
    /// The model-facing description.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// The model.
    pub fn model(mut self, model: Arc<dyn Llm>) -> Self {
        self.model = Some(model);
        self
    }

    /// The egress gate every crop for a remote model passes; required for one.
    pub fn egress_gate(mut self, gate: Arc<EgressGate>) -> Self {
        self.gate = Some(gate);
        self
    }

    /// The hosts the model sends to, for the egress policy and records; the model's name by
    /// default.
    pub fn hosts(mut self, hosts: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.hosts = hosts.into_iter().map(Into::into).collect();
        self
    }

    /// The data class crops are released as; `team_crop` by default.
    pub fn data_class(mut self, class: &str) -> Self {
        self.data_class = class.to_string();
        self
    }

    /// The provider, when the model's id does not name it ([`provider_of`]).
    pub fn provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// The most output tokens an answer may take.
    pub fn max_output_tokens(mut self, tokens: i32) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }

    /// The generation settings every request starts from, such as a provider extension.
    pub fn generate_config(mut self, config: adk_core::GenerateContentConfig) -> Self {
        self.config = config;
        self
    }

    /// Builds the reasoner.
    ///
    /// # Errors
    ///
    /// [`TeamRoleError::Missing`] without a model, or for a remote model without an egress
    /// gate.
    pub fn build(self) -> std::result::Result<Reasoner, TeamRoleError> {
        let model = self.model.ok_or_else(|| {
            TeamRoleError::Missing(format!("the reasoner '{}' needs a model", self.name))
        })?;
        let remote = model.data_locality().leaves_machine();
        if remote && self.gate.is_none() {
            return Err(TeamRoleError::Missing(format!(
                "the reasoner '{}' runs on the remote model '{}' and needs the egress gate its \
                 crops pass (ReasonerBuilder::egress_gate)",
                self.name,
                model.name()
            )));
        }
        let hosts = if self.hosts.is_empty() { vec![model.name().to_string()] } else { self.hosts };
        let locality =
            if remote { Locality::Remote { hosts: hosts.clone() } } else { Locality::InProcess };
        let destination = BackendDescriptor::new(model.name(), locality, CoordFrame::SentPixels);
        let provider = provider_of(model.name(), self.provider.as_deref());
        #[cfg(feature = "guardrail")]
        let guardrail = self.gate.as_ref().map(|gate| {
            crate::guardrail::ImagePrivacyGuardrail::for_model(
                Arc::clone(gate),
                model.as_ref(),
                hosts.clone(),
            )
        });
        Ok(Reasoner {
            name: self.name,
            description: self.description,
            model,
            gate: self.gate,
            destination,
            data_class: DataClass::new(self.data_class),
            provider,
            #[cfg(feature = "guardrail")]
            guardrail,
            max_output_tokens: self.max_output_tokens,
            config: self.config,
        })
    }
}

#[async_trait]
impl Agent for Reasoner {
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
        let question = request.get("question").and_then(Value::as_str).unwrap_or_default();
        let regions: RegionSet = request
            .get("regions")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or_else(|| AdkError::agent("a reasoner request needs `regions`, a RegionSet"))?;
        let measurements: Vec<Value> =
            request.get("measurements").and_then(Value::as_array).cloned().unwrap_or_default();
        let verdicts: Vec<String> = match request.get("verdicts").and_then(Value::as_array) {
            Some(items) => items.iter().filter_map(Value::as_str).map(str::to_string).collect(),
            None => vec!["yes".to_string(), "no".to_string()],
        };
        let mut parts = Vec::new();
        for crop in request.get("crops").and_then(Value::as_array).into_iter().flatten() {
            let name = crop.as_str().unwrap_or_default();
            let digest = name
                .strip_prefix("sha256:")
                .and_then(|hex| Digest::from_hex(hex).ok())
                .ok_or_else(|| AdkError::agent(format!("`{name}` is not an image digest")))?;
            // Only an image this reasoner's gate released may reach its model.
            if self.is_remote() && !self.gate.as_ref().is_some_and(|g| g.has_released(&digest)) {
                return Err(
                    VisionError::NotReleased { backend: self.destination.id.to_string() }.into()
                );
            }
            let data = super::load_bytes(ctx.as_ref(), name).await?;
            parts.push(Part::InlineData {
                mime_type: "image/png".into(),
                data,
                uri: None,
                annotations: Some(json!({
                    "adkVision": { "digest": digest, "release": { "egress": digest } }
                })),
            });
        }
        let cx = cx_for(ctx.as_ref());
        let opinion = self.ask(&cx, question, &verdicts, &regions, &measurements, parts).await?;
        let text = serde_json::to_string(&opinion)
            .map_err(|err| AdkError::agent(format!("the opinion could not be encoded: {err}")))?;
        let event = text_event(ctx.as_ref(), &self.name, text);
        Ok(Box::pin(futures::stream::iter([Ok(event)])))
    }
}

/// Whether the egress rules release an image of `class` to `destination` without asking.
fn allows(gate: &EgressGate, destination: &BackendDescriptor, class: &DataClass) -> bool {
    match &destination.locality {
        Locality::Remote { hosts } => {
            gate.decide(hosts, class).0 == crate::egress::EgressAction::Allow
        }
        _ => true,
    }
}

fn with_margin(crop: BBox, source: &SourceImage) -> BBox {
    let (w, h) = source.size();
    crop.expanded(CROP_MARGIN).clamped(w, h)
}

// ---------------------------------------------------------------------------
// Verifier
// ---------------------------------------------------------------------------

/// What the verifier's on-device check found for one claim.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RegionCheck {
    /// The class looked for.
    pub label: String,
    /// The claimed region, in source pixels.
    pub claimed: BBox,
    /// What local perception found for the class, in source pixels.
    pub found: Vec<Region>,
    /// The best overlap of a found region with the claimed one.
    pub best_iou: f32,
    /// Whether a found region overlaps the claim enough to confirm it.
    pub confirmed: bool,
}

/// What the coordinator asks a verifier: re-check `claim` about `region` of `source`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct VerifyRequest<'a> {
    /// The question the claim answers.
    pub question: String,
    /// The two verdicts: the claim holds, the claim does not.
    pub verdicts: [String; 2],
    /// The claim to re-check.
    pub claim: Opinion,
    /// The class to look for; the claim's label when `None`.
    pub label: Option<String>,
    /// The claimed region, in source pixels.
    pub region: BBox,
    /// The source image on the device.
    pub source: &'a SourceImage,
    /// Egress digests a person approved, for a remote verifier model's crop.
    pub approved: Vec<Digest>,
}

impl<'a> VerifyRequest<'a> {
    /// A request to re-check `claim` about `region` of `source`; `positive` is the verdict
    /// that says the class is there and `negative` the one that says it is not.
    pub fn new(
        question: impl Into<String>,
        positive: impl Into<String>,
        negative: impl Into<String>,
        claim: Opinion,
        region: BBox,
        source: &'a SourceImage,
    ) -> Self {
        Self {
            question: question.into(),
            verdicts: [positive.into(), negative.into()],
            claim,
            label: None,
            region,
            source,
            approved: Vec::new(),
        }
    }

    /// Looks for `label` instead of the claim's.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// A verifier's answer, its on-device check and the receipt of any crop it sent.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Verified {
    /// The opinion.
    pub opinion: Opinion,
    /// What local perception found.
    pub check: RegionCheck,
    /// The receipt of the crop a remote verifier model was sent.
    pub released: Option<EgressReceipt>,
}

/// The verifier: re-checks a claim against the pixels with local perception, optionally with
/// a model, by default on another provider than the reasoner's (spec 3.7 §7.12).
///
/// The check runs the verifier's on-device detector for the claimed class and compares the
/// regions it finds with the claimed one by overlap. Without a model, the check decides; with
/// one, the model sees the claim, the check and a crop of the region — released through the
/// egress gate when the model is remote — and decides.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_vision::team::Verifier;
/// use adk_vision::{
///     BackendDescriptor, CoordFrame, DetectQuery, Detector, Locality, RawRegion, SentImage,
///     VisionCx, VisionRuntime,
/// };
///
/// struct Nothing(BackendDescriptor);
///
/// #[async_trait::async_trait]
/// impl Detector for Nothing {
///     fn descriptor(&self) -> &BackendDescriptor { &self.0 }
///     async fn detect(&self, _: &VisionCx, _: &SentImage, _: &DetectQuery) -> adk_vision::Result<Vec<RawRegion>> {
///         Ok(Vec::new())
///     }
/// }
///
/// let local = BackendDescriptor::new("strict", Locality::InProcess, CoordFrame::SentPixels);
/// let runtime = VisionRuntime::builder().detector(Arc::new(Nothing(local))).build().unwrap();
/// let verifier = Verifier::builder("verifier").runtime(Arc::new(runtime)).build().unwrap();
/// assert_eq!(verifier.provider(), "local");
/// ```
pub struct Verifier {
    name: String,
    description: String,
    runtime: Arc<VisionRuntime>,
    model: Option<Arc<dyn Llm>>,
    gate: Option<Arc<EgressGate>>,
    destination: Option<BackendDescriptor>,
    data_class: DataClass,
    provider: String,
    iou: f32,
    min_score: f32,
    max_output_tokens: Option<i32>,
    config: adk_core::GenerateContentConfig,
}

impl std::fmt::Debug for Verifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Verifier")
            .field("name", &self.name)
            .field("model", &self.model.as_ref().map(|m| m.name().to_string()))
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

/// Builds a [`Verifier`].
#[must_use]
pub struct VerifierBuilder {
    name: String,
    description: String,
    runtime: Option<Arc<VisionRuntime>>,
    model: Option<Arc<dyn Llm>>,
    gate: Option<Arc<EgressGate>>,
    hosts: Vec<String>,
    data_class: String,
    provider: Option<String>,
    iou: f32,
    min_score: f32,
    max_output_tokens: Option<i32>,
    config: adk_core::GenerateContentConfig,
}

impl Verifier {
    /// A builder for a verifier named `name`.
    pub fn builder(name: impl Into<String>) -> VerifierBuilder {
        VerifierBuilder {
            name: name.into(),
            description: "Re-checks a claim against the pixels with local perception.".to_string(),
            runtime: None,
            model: None,
            gate: None,
            hosts: Vec::new(),
            data_class: "team_crop".to_string(),
            provider: None,
            iou: 0.3,
            min_score: 0.25,
            max_output_tokens: None,
            config: adk_core::GenerateContentConfig::default(),
        }
    }

    /// The member name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The provider the verifier decides on: its model's, or `local` for a check alone.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Whether the verifier's model requests leave the machine.
    pub fn is_remote(&self) -> bool {
        self.model.as_ref().is_some_and(|m| m.data_locality().leaves_machine())
    }

    /// The egress gate a remote verifier model's crops pass.
    pub fn egress_gate(&self) -> Option<&Arc<EgressGate>> {
        self.gate.as_ref()
    }

    /// Stages a crop for a remote verifier model when its egress rule asks, as
    /// [`Reasoner::stage`] does. `None` when nothing would leave or the rule allows it.
    ///
    /// # Errors
    ///
    /// The gate's refusals.
    pub async fn stage(
        &self,
        cx: &VisionCx,
        source: &SourceImage,
        crop: BBox,
    ) -> Result<Option<StagedEgress>> {
        let (Some(gate), Some(destination)) = (&self.gate, &self.destination) else {
            return Ok(None);
        };
        if !self.is_remote() || allows(gate, destination, &self.data_class) {
            return Ok(None);
        }
        let request = ReleaseRequest::new(source, destination, self.data_class.clone())
            .with_crop(with_margin(crop, source));
        gate.stage(cx, &request).await.map(Some)
    }

    /// Runs the on-device check for `label` around `region`.
    ///
    /// # Errors
    ///
    /// The runtime's error.
    pub async fn check(
        &self,
        cx: &VisionCx,
        source: &SourceImage,
        label: &str,
        region: BBox,
    ) -> Result<RegionCheck> {
        let found = self
            .runtime
            .detect(cx, source, &DetectQuery::Classes(vec![label.to_string()]))
            .await?
            .value;
        let found: Vec<Region> = found
            .into_iter()
            .filter(|r| r.label.as_deref().is_none_or(|l| l == label))
            .filter(|r| r.score.is_none_or(|s| s >= self.min_score))
            .collect();
        let best_iou = found
            .iter()
            .filter_map(|r| r.shape.bbox())
            .map(|bbox| bbox.iou(&region))
            .fold(0.0_f32, f32::max);
        Ok(RegionCheck {
            label: label.to_string(),
            claimed: region,
            found,
            best_iou,
            confirmed: best_iou >= self.iou,
        })
    }

    /// Re-checks a claim.
    ///
    /// # Errors
    ///
    /// The check's error, the gate's refusals for a remote model's crop, the model's error,
    /// and [`VisionError::Parse`] for an answer that is not an opinion with one of the two
    /// verdicts.
    pub async fn verify(&self, cx: &VisionCx, request: VerifyRequest<'_>) -> Result<Verified> {
        let label = request
            .label
            .clone()
            .or_else(|| request.claim.label.clone())
            .unwrap_or_else(|| request.verdicts[0].clone());
        let check = self.check(cx, request.source, &label, request.region).await?;
        let Some(model) = &self.model else {
            let verdict = if check.confirmed { &request.verdicts[0] } else { &request.verdicts[1] };
            let confidence = if check.confirmed { check.best_iou } else { 1.0 - check.best_iou };
            let mut opinion = Opinion::new(verdict.clone(), confidence).with_label(label.clone());
            opinion.rationale = if check.confirmed {
                format!(
                    "local perception finds `{label}` over the claimed region (overlap {:.2})",
                    check.best_iou
                )
            } else {
                format!(
                    "local perception finds no `{label}` over the claimed region (best overlap \
                     {:.2}, {} found elsewhere)",
                    check.best_iou,
                    check.found.len()
                )
            };
            opinion.member = self.name.clone();
            opinion.provider = self.provider.clone();
            return Ok(Verified { opinion, check, released: None });
        };
        let crop = with_margin(request.region, request.source);
        let mut released = None;
        let image = if self.is_remote() {
            let (Some(gate), Some(destination)) = (&self.gate, &self.destination) else {
                return Err(VisionError::NotReleased { backend: model.name().to_string() });
            };
            let mut release =
                ReleaseRequest::new(request.source, destination, self.data_class.clone())
                    .with_crop(crop);
            for digest in &request.approved {
                release = release.approved(*digest);
            }
            let image = gate.release(cx, release).await?;
            released = Some(image.receipt().clone());
            image.to_part()
        } else {
            let (w, h) = request.source.size();
            let (x, y, cw, ch) = crop.pixel_bounds(w, h);
            Part::InlineData {
                mime_type: "image/png".into(),
                data: crate::edit::ops::crop(request.source.pixels(), x, y, cw, ch).encode_png()?,
                uri: None,
                annotations: None,
            }
        };
        let verdicts = request.verdicts.to_vec();
        let text = format!(
            "You are the verifier of a vision team. Another model made a claim about a region of \
             an image; check it against the pixels. The image is a crop around the claimed \
             region, redacted before it was sent.\n\n\
             Question: {question}\n\
             Claim: {claim}\n\
             Local perception on the device looked for `{label}` around the claimed region: \
             {found} region(s) found, best overlap with the claim {iou:.2}.\n\n\
             Answer with JSON only: {{\"verdict\": <one of {list}>, \"label\": <the class>, \
             \"confidence\": <0 to 1>, \"rationale\": <one sentence>}}.",
            question = request.question,
            claim = serde_json::to_string(&request.claim).unwrap_or_default(),
            found = check.found.len(),
            iou = check.best_iou,
            list = verdicts.iter().map(|v| format!("\"{v}\"")).collect::<Vec<_>>().join(", "),
        );
        let mut content = Content { role: "user".into(), parts: vec![image, Part::Text { text }] };
        #[cfg(feature = "guardrail")]
        if let (Some(gate), true) = (&self.gate, self.is_remote()) {
            let hosts = match self.destination.as_ref().map(|d| &d.locality) {
                Some(Locality::Remote { hosts }) => hosts.clone(),
                _ => vec![model.name().to_string()],
            };
            crate::guardrail::ImagePrivacyGuardrail::for_model(
                Arc::clone(gate),
                model.as_ref(),
                hosts,
            )
            .gate_content(cx, &mut content)
            .await;
        }
        let mut llm_request = LlmRequest::new(model.name(), vec![content]);
        let mut config = self.config.clone();
        if self.max_output_tokens.is_some() {
            config.max_output_tokens = self.max_output_tokens;
        }
        llm_request.config = Some(config);
        let answer = answer_text(cx, model, llm_request).await?;
        let opinion = parse_opinion(model.name(), &answer, &verdicts, &self.name, &self.provider)?;
        Ok(Verified { opinion, check, released })
    }
}

impl VerifierBuilder {
    /// The model-facing description.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// The runtime the check perceives with; every backend must be on the device. Required.
    pub fn runtime(mut self, runtime: Arc<VisionRuntime>) -> Self {
        self.runtime = Some(runtime);
        self
    }

    /// A model that decides with the check; any provider, by default another than the
    /// reasoner's.
    pub fn model(mut self, model: Arc<dyn Llm>) -> Self {
        self.model = Some(model);
        self
    }

    /// The egress gate a remote model's crops pass; required for one.
    pub fn egress_gate(mut self, gate: Arc<EgressGate>) -> Self {
        self.gate = Some(gate);
        self
    }

    /// The hosts a remote model sends to; the model's name by default.
    pub fn hosts(mut self, hosts: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.hosts = hosts.into_iter().map(Into::into).collect();
        self
    }

    /// The data class crops are released as; `team_crop` by default.
    pub fn data_class(mut self, class: &str) -> Self {
        self.data_class = class.to_string();
        self
    }

    /// The provider, when the model's id does not name it ([`provider_of`]).
    pub fn provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// The overlap a found region needs with the claim to confirm it; 0.3 by default.
    pub fn min_iou(mut self, iou: f32) -> Self {
        self.iou = iou;
        self
    }

    /// The score a found region needs to count; 0.25 by default.
    pub fn min_score(mut self, score: f32) -> Self {
        self.min_score = score;
        self
    }

    /// The most output tokens a model answer may take.
    pub fn max_output_tokens(mut self, tokens: i32) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }

    /// The generation settings every model request starts from, such as a provider
    /// extension.
    pub fn generate_config(mut self, config: adk_core::GenerateContentConfig) -> Self {
        self.config = config;
        self
    }

    /// Builds the verifier.
    ///
    /// # Errors
    ///
    /// [`TeamRoleError::Missing`] without a runtime, or for a remote model without an egress
    /// gate, and [`TeamRoleError::RemoteBackend`] for a runtime with a remote backend.
    pub fn build(self) -> std::result::Result<Verifier, TeamRoleError> {
        let runtime = self.runtime.ok_or_else(|| {
            TeamRoleError::Missing(format!(
                "the verifier '{}' needs a runtime of on-device backends to check claims with",
                self.name
            ))
        })?;
        check_local_runtime("verifier", &self.name, &runtime)?;
        let remote = self.model.as_ref().is_some_and(|m| m.data_locality().leaves_machine());
        if remote && self.gate.is_none() {
            return Err(TeamRoleError::Missing(format!(
                "the verifier '{}' runs on a remote model and needs the egress gate its crops \
                 pass (VerifierBuilder::egress_gate)",
                self.name
            )));
        }
        let destination = self.model.as_ref().map(|model| {
            let locality = if remote {
                Locality::Remote {
                    hosts: if self.hosts.is_empty() {
                        vec![model.name().to_string()]
                    } else {
                        self.hosts.clone()
                    },
                }
            } else {
                Locality::InProcess
            };
            BackendDescriptor::new(model.name(), locality, CoordFrame::SentPixels)
        });
        let provider = match &self.model {
            Some(model) => provider_of(model.name(), self.provider.as_deref()),
            None => self.provider.unwrap_or_else(|| "local".to_string()),
        };
        Ok(Verifier {
            name: self.name,
            description: self.description,
            runtime,
            model: self.model,
            gate: self.gate,
            destination,
            data_class: DataClass::new(self.data_class),
            provider,
            iou: self.iou,
            min_score: self.min_score,
            max_output_tokens: self.max_output_tokens,
            config: self.config,
        })
    }
}

#[async_trait]
impl Agent for Verifier {
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
        let claim: Opinion = request
            .get("claim")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or_else(|| AdkError::agent("a verifier request needs `claim`, an opinion"))?;
        let image = request.get("image").and_then(Value::as_str).unwrap_or_default();
        let source = load_image(ctx.as_ref(), image).await?;
        let regions: Option<RegionSet> =
            request.get("regions").cloned().and_then(|value| serde_json::from_value(value).ok());
        let region = regions
            .as_ref()
            .and_then(|set| set.regions.first())
            .or_else(|| claim.regions.as_ref().and_then(|set| set.regions.first()))
            .and_then(|region| region.shape.bbox())
            .ok_or_else(|| AdkError::agent("a verifier request needs the claimed region"))?;
        let question = request.get("question").and_then(Value::as_str).unwrap_or_default();
        let positive = claim.verdict.clone();
        let negative = format!("not_{positive}");
        let cx = cx_for(ctx.as_ref());
        let verified = self
            .verify(&cx, VerifyRequest::new(question, positive, negative, claim, region, &source))
            .await?;
        let text = serde_json::to_string(&verified.opinion)
            .map_err(|err| AdkError::agent(format!("the opinion could not be encoded: {err}")))?;
        let event = text_event(ctx.as_ref(), &self.name, text);
        Ok(Box::pin(futures::stream::iter([Ok(event)])))
    }
}
