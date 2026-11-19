//! [`VisionToolset`]: the governed vision tools (spec 3.7 §7.8).
//!
//! Every tool is an `adk_core::Tool`, so an agent dispatches it through
//! `adk_core::authorize_tool_call`, the one governed path: policy, guardrails, confirmation,
//! the kill switch, the action ledger and the signed audit trail apply as they do to any
//! tool. On top of that:
//!
//! - **Destinations.** A call routed to a remote backend declares the backend's hosts as its
//!   `ToolEgress`, so the run's egress policy refuses an unlisted host before anything runs.
//! - **Approve with the image.** [`VisionToolset::evidence_callback`] stages what a remote
//!   call would send — destination checked, cropped, redacted, the copy stored as an
//!   artifact — before the call is authorized, and the tool's
//!   [`approval_evidence`](adk_core::Tool::approval_evidence) hands the governed path that
//!   exact image. Its digest joins the call's fingerprint, so a decision binds to it and a
//!   changed image holds the call again (V-I5).
//! - **The egress rule decides.** [`VisionToolset::egress_policy`] is a `ToolPolicy` that
//!   applies `.adk/vision.toml` per call: `deny` refuses, `ask` holds for a person, `allow`
//!   lets the call run. A call held under `ask` and then approved releases exactly the
//!   digests its evidence named; nothing else leaves.
//! - **Budgets and audit.** Each backend call opens its hold before any image is released
//!   and records its units; every call writes `vision.call`, every release
//!   `vision.egress.released`, every edit `vision.edit` to the run's audit trail.
//!
//! | Tool | Does | Effect |
//! |------|------|--------|
//! | `vision_detect` | Objects by class or concept | `ReadOnly` |
//! | `vision_segment` | Masks from concepts, boxes or points | `ReadOnly` |
//! | `vision_read` | Text, tables and fields of a document page | `ReadOnly` |
//! | `vision_describe` | An answer about the image, as text or a value matching a schema | `ReadOnly` |
//! | `vision_crop` | A new image of a region | `Idempotent` |
//! | `vision_redact` | A new image with regions filled solid black | `Idempotent` |
//! | `vision_annotate` | A new image with labelled boxes drawn | `Idempotent` |

mod tools;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use adk_core::audit_log::AuditActor;
use adk_core::{
    Artifacts, BeforeToolCallback, CallbackContext, Content, ImageEvidence, ModelCallScope, Part,
    PolicyCall, PolicyDecision, ReadonlyContext, Tool, ToolContext, ToolPolicy, ToolPolicyRequest,
    Toolset,
};
use async_trait::async_trait;
use serde_json::Value;

pub use tools::{VISION_TOOL_NAMES, VisionTool, VisionToolKind};

use crate::backend::BackendDescriptor;
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::egress::{DataClass, EgressAction, ReleaseRequest};
use crate::error::{Result, VisionError};
use crate::geometry::BBox;
use crate::media::{ImageRef, MediaScope, MediaStore, Provenance, SourceImage};
use crate::runtime::{CallOptions, Route, VisionRuntime};

/// How an `ask` egress rule is satisfied for a call a vision tool makes.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EgressApproval {
    /// The governed path asks. [`VisionToolset::egress_policy`], installed on the run, holds
    /// every call whose rule asks, and [`VisionToolset::evidence_callback`] puts the exact
    /// egress image on the held call. A call that then executes releases exactly the digests
    /// its evidence named. A call the policy did not hold, or one without staged evidence,
    /// is refused, so a run missing either piece fails closed.
    #[default]
    GovernedPath,
    /// Only digests approved through [`VisionToolset::approve_egress`] are released, for
    /// applications that show the image and record the decision themselves.
    Explicit,
}

/// What [`VisionToolset::evidence_callback`] staged for one call: the egress digests and the
/// evidence an approver sees.
#[derive(Clone, Debug)]
struct StagedCall {
    digests: Vec<Digest>,
    evidence: Vec<ImageEvidence>,
}

pub(crate) struct Inner {
    runtime: Arc<VisionRuntime>,
    store: MediaStore,
    approval: EgressApproval,
    image_class: DataClass,
    crop_class: DataClass,
    images: Mutex<HashMap<(MediaScope, Digest), ImageRef>>,
    staged: Mutex<HashMap<String, StagedCall>>,
    governed: Mutex<HashMap<String, EgressAction>>,
    approved: Mutex<HashSet<Digest>>,
}

/// One planned call: its route, the primary backend's descriptor, the images it names,
/// the crop the gate sends and the data class it is released as.
struct Plan {
    route: Route,
    descriptor: BackendDescriptor,
    images: Vec<String>,
    crop: Option<BBox>,
    class: DataClass,
}

impl Plan {
    fn is_remote(&self) -> bool {
        self.descriptor.locality.is_remote()
    }
}

/// The governed vision tools over a [`VisionRuntime`].
///
/// Images are named by their `sha256:` digest, as the tools return them, or by the name of
/// an artifact in the session; the toolset stores each one it reads in the runtime's media
/// store, by digest. Install three pieces on the agent and run:
///
/// | Piece | Where | Why |
/// |-------|-------|-----|
/// | The toolset | `LlmAgentBuilder::toolset` | The tools |
/// | [`evidence_callback`](Self::evidence_callback) | `LlmAgentBuilder::before_tool_callback` | A held remote call carries its egress image |
/// | [`egress_policy`](Self::egress_policy) | `RunConfig::tool_policy` or `Runner::builder().tool_policy` | `.adk/vision.toml` decides each remote call: deny, ask or allow |
///
/// The `adk-harness` vision pack installs all three.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_artifact::InMemoryArtifactService;
/// use adk_vision::{DeterministicEditor, MediaStore, VisionRuntime, VisionToolset};
///
/// let store = MediaStore::new(Arc::new(InMemoryArtifactService::new()));
/// let runtime = VisionRuntime::builder()
///     .editor(Arc::new(DeterministicEditor::new()))
///     .media_store(store)
///     .build()
///     .unwrap();
/// let toolset = VisionToolset::builder(Arc::new(runtime)).build().unwrap();
/// // Only the tasks the runtime has a backend for become tools.
/// assert_eq!(toolset.tool_names(), vec!["vision_crop", "vision_redact", "vision_annotate"]);
/// ```
#[derive(Clone)]
pub struct VisionToolset {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for VisionToolset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VisionToolset")
            .field("tools", &self.tool_names())
            .field("approval", &self.inner.approval)
            .finish_non_exhaustive()
    }
}

/// Builds a [`VisionToolset`].
#[must_use]
pub struct VisionToolsetBuilder {
    runtime: Arc<VisionRuntime>,
    approval: EgressApproval,
    image_class: DataClass,
    crop_class: DataClass,
}

impl VisionToolsetBuilder {
    /// How `ask` rules are satisfied; [`EgressApproval::GovernedPath`] by default.
    pub fn approval(mut self, approval: EgressApproval) -> Self {
        self.approval = approval;
        self
    }

    /// The data classes images are released as: `image` for a whole image and `crop` for a
    /// call that names a region. `image` and `image_crop` by default. The operator sets
    /// them, never the model, since the egress rules are written per class.
    pub fn data_classes(mut self, image: &str, crop: &str) -> Self {
        self.image_class = DataClass::new(image);
        self.crop_class = DataClass::new(crop);
        self
    }

    /// Builds the toolset.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] when the runtime has no media store: the tools keep
    /// every image they read and make as an artifact.
    pub fn build(self) -> Result<VisionToolset> {
        let store = self.runtime.media_store().cloned().ok_or_else(|| {
            VisionError::InvalidInput(
                "the vision tools keep images as artifacts; give the runtime a media store \
                 (VisionRuntimeBuilder::media_store)"
                    .into(),
            )
        })?;
        Ok(VisionToolset {
            inner: Arc::new(Inner {
                runtime: self.runtime,
                store,
                approval: self.approval,
                image_class: self.image_class,
                crop_class: self.crop_class,
                images: Mutex::new(HashMap::new()),
                staged: Mutex::new(HashMap::new()),
                governed: Mutex::new(HashMap::new()),
                approved: Mutex::new(HashSet::new()),
            }),
        })
    }
}

impl VisionToolset {
    /// A builder over `runtime`, which needs a media store.
    pub fn builder(runtime: Arc<VisionRuntime>) -> VisionToolsetBuilder {
        VisionToolsetBuilder {
            runtime,
            approval: EgressApproval::default(),
            image_class: DataClass::new("image"),
            crop_class: DataClass::new("image_crop"),
        }
    }

    /// The runtime the tools call.
    pub fn runtime(&self) -> &Arc<VisionRuntime> {
        &self.inner.runtime
    }

    /// The tools, one per task the runtime has a backend for.
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        VisionToolKind::ALL
            .iter()
            .filter(|kind| self.inner.serves(**kind))
            .map(|kind| Arc::new(VisionTool::new(*kind, self.inner.clone())) as Arc<dyn Tool>)
            .collect()
    }

    /// The names of [`tools`](Self::tools).
    pub fn tool_names(&self) -> Vec<&'static str> {
        VisionToolKind::ALL
            .iter()
            .filter(|kind| self.inner.serves(**kind))
            .map(|kind| kind.name())
            .collect()
    }

    /// Whether `name` is one of this toolset's tools.
    pub fn is_tool(&self, name: &str) -> bool {
        self.tool_names().contains(&name)
    }

    /// Approves an egress digest for [`EgressApproval::Explicit`]: the next call whose
    /// staged image has this digest may release it.
    pub fn approve_egress(&self, digest: Digest) {
        lock(&self.inner.approved).insert(digest);
    }

    /// Whether a call of `tool` with `args` would send an image off the machine: its route's
    /// primary backend is remote.
    pub fn is_remote_call(&self, tool: &str, args: &Value) -> bool {
        VisionToolKind::from_name(tool)
            .and_then(|kind| self.inner.plan(kind, args).ok())
            .is_some_and(|plan| plan.is_remote())
    }

    /// What the egress rules say about a call of `tool` with `args`: `None` for a call that
    /// stays on the device (or does not parse), else the rule's action for its destination
    /// and data class. A harness classes the call by it (spec 3.7 X15).
    pub fn egress_action(&self, tool: &str, args: &Value) -> Option<EgressAction> {
        let kind = VisionToolKind::from_name(tool)?;
        let plan = self.inner.plan(kind, args).ok()?;
        self.inner.egress_action(&plan).map(|(action, _)| action)
    }

    /// The `BeforeToolCallback` that stages what a remote call would send, so the call is
    /// held with the exact egress image (spec 3.7 S4).
    ///
    /// For a vision tool call routed to a remote backend it resolves the image, runs the
    /// egress gate's destination, minimise and redact steps, stores the egress copy, and
    /// keeps the copy's evidence for the tool's `approval_evidence`. It answers no call it
    /// can stage; a call the gate refuses outright — an unlisted host, a missing redactor, a
    /// rule that denies — is answered with the refusal, so nobody is asked about an image
    /// that can never leave.
    pub fn evidence_callback(&self) -> BeforeToolCallback {
        let inner = self.inner.clone();
        Box::new(move |ctx: Arc<dyn CallbackContext>| {
            let inner = inner.clone();
            Box::pin(async move { inner.stage_call(ctx).await })
        })
    }

    /// The `ToolPolicy` that applies the egress rules to each vision call: `deny` refuses,
    /// `ask` requires a person's approval, `allow` and on-device calls are allowed. Compose
    /// it with the run's other policies (`StrictestPolicy`); it decides no other tool.
    pub fn egress_policy(&self) -> Arc<dyn ToolPolicy> {
        Arc::new(VisionEgressPolicy { inner: self.inner.clone() })
    }
}

#[async_trait]
impl Toolset for VisionToolset {
    fn name(&self) -> &str {
        "vision"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> adk_core::Result<Vec<Arc<dyn Tool>>> {
        Ok(VisionToolset::tools(self))
    }
}

/// Applies `.adk/vision.toml` to each vision tool call; see [`VisionToolset::egress_policy`].
struct VisionEgressPolicy {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for VisionEgressPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VisionEgressPolicy").finish_non_exhaustive()
    }
}

/// `policy_ref` of the vision egress policy in audit records.
pub const VISION_EGRESS_POLICY_REF: &str = "adk-vision-egress";

#[async_trait]
impl ToolPolicy for VisionEgressPolicy {
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        self.evaluate_call(request, &PolicyCall::new()).await
    }

    async fn evaluate_call(
        &self,
        request: &ToolPolicyRequest,
        call: &PolicyCall<'_>,
    ) -> PolicyDecision {
        let Some(kind) = VisionToolKind::from_name(&request.tool_name) else {
            return PolicyDecision::Allow;
        };
        if !self.inner.serves(kind) {
            return PolicyDecision::Allow;
        }
        // A call whose arguments do not parse is refused by the tool itself.
        let Ok(plan) = self.inner.plan(kind, &request.args) else {
            return PolicyDecision::Allow;
        };
        let Some((action, hosts)) = self.inner.egress_action(&plan) else {
            return PolicyDecision::Allow;
        };
        let key = governed_key(call.function_call_id.unwrap_or_default(), kind, &request.args);
        lock(&self.inner.governed).insert(key, action);
        match action {
            EgressAction::Allow => PolicyDecision::Allow,
            EgressAction::Ask => PolicyDecision::require_approval(format!(
                "`{}` would send an image to {} as `{}`; the egress rule asks a person first",
                request.tool_name,
                hosts.join(", "),
                plan.class
            )),
            EgressAction::Deny => PolicyDecision::deny(format!(
                "the egress rules deny sending `{}` images to {}",
                plan.class,
                hosts.join(", ")
            )),
        }
    }

    fn policy_ref(&self) -> Option<String> {
        Some(VISION_EGRESS_POLICY_REF.to_string())
    }
}

impl Inner {
    /// Whether the runtime has a backend for `kind`'s task.
    fn serves(&self, kind: VisionToolKind) -> bool {
        self.runtime.route(kind.task(), &CallOptions::new()).is_ok()
    }

    fn plan(&self, kind: VisionToolKind, args: &Value) -> Result<Plan> {
        let task = kind.task();
        let mut options = CallOptions::new();
        if let Some(backend) = args.get("backend").and_then(Value::as_str) {
            options = options.with_route(Route::to(backend));
        }
        let route = self.runtime.route(task, &options)?;
        let descriptor = self
            .runtime
            .descriptor(task, &route.primary)
            .cloned()
            .ok_or_else(|| VisionError::BackendNotFound(route.primary.to_string()))?;
        let image = args
            .get("image")
            .and_then(Value::as_str)
            .ok_or_else(|| VisionError::InvalidInput("`image` is required".into()))?;
        // Edits keep their region as the operation's; only perception crops what it sends.
        let crop = match kind.is_edit() {
            true => None,
            false => args.get("region").map(tools::bbox_arg).transpose()?,
        };
        let class = if crop.is_some() { self.crop_class.clone() } else { self.image_class.clone() };
        Ok(Plan { route, descriptor, images: vec![image.to_string()], crop, class })
    }

    /// The egress rule's action for a remote plan, and the destination hosts; `None` for a
    /// plan that stays on the device.
    fn egress_action(&self, plan: &Plan) -> Option<(EgressAction, Vec<String>)> {
        let hosts = match &plan.descriptor.locality {
            crate::backend::Locality::Remote { hosts } => hosts.clone(),
            _ => return None,
        };
        let action = match self.runtime.egress_gate() {
            Some(gate) => gate.decide(&hosts, &plan.class).0,
            // Without a gate the runtime refuses every remote call.
            None => EgressAction::Deny,
        };
        Some((action, hosts))
    }

    /// Resolves an image argument: a `sha256:` digest of an image this session holds, or
    /// the name of an artifact in the session.
    async fn resolve(
        &self,
        artifacts: Option<&Arc<dyn Artifacts>>,
        scope: &MediaScope,
        image: &str,
    ) -> Result<SourceImage> {
        if let Some(hex) = image.strip_prefix("sha256:") {
            let digest = Digest::from_hex(hex)?;
            let known = lock(&self.images).get(&(scope.clone(), digest)).cloned();
            if let Some(known) = known {
                return self.store.load(&known).await;
            }
            // The media store names every image it keeps by digest.
            if let Some(artifacts) = artifacts {
                for extension in ["png", "jpeg", "webp", "gif"] {
                    let name = format!("vision-{hex}.{extension}");
                    if let Ok(part) = artifacts.load(&name).await {
                        return self.adopt(scope, part, Some(digest), &name).await;
                    }
                }
            }
            return Err(VisionError::InvalidInput(format!(
                "this session holds no image with digest {image}; name an artifact or a digest \
                 an earlier vision tool returned"
            )));
        }
        let artifacts = artifacts.ok_or_else(|| {
            VisionError::InvalidInput(format!(
                "`{image}` names an artifact, and this run has no artifact service"
            ))
        })?;
        let part = artifacts.load(image).await.map_err(|err| {
            VisionError::InvalidInput(format!("artifact `{image}` could not be loaded: {err}"))
        })?;
        self.adopt(scope, part, None, image).await
    }

    async fn adopt(
        &self,
        scope: &MediaScope,
        part: Part,
        expected: Option<Digest>,
        name: &str,
    ) -> Result<SourceImage> {
        let Part::InlineData { data, .. } = part else {
            return Err(VisionError::InvalidInput(format!(
                "artifact `{name}` does not hold an image"
            )));
        };
        if let Some(expected) = expected {
            let actual = Digest::of(&data);
            if actual != expected {
                return Err(VisionError::DigestMismatch { path: name.into(), expected, actual });
            }
        }
        let (image, source) = self.store.put_image(scope, data, Provenance::Upload).await?;
        self.remember(scope, image);
        Ok(source)
    }

    fn remember(&self, scope: &MediaScope, image: ImageRef) {
        lock(&self.images).insert((scope.clone(), image.digest), image);
    }

    /// Stages a remote call's egress images and keeps their evidence; see
    /// [`VisionToolset::evidence_callback`].
    async fn stage_call(&self, ctx: Arc<dyn CallbackContext>) -> adk_core::Result<Option<Content>> {
        let Some(kind) = ctx.tool_name().and_then(VisionToolKind::from_name) else {
            return Ok(None);
        };
        let args = ctx.tool_input().cloned().unwrap_or(Value::Null);
        let key = call_key(kind, &args);
        lock(&self.staged).remove(&key);
        if !self.serves(kind) {
            return Ok(None);
        }
        let Ok(plan) = self.plan(kind, &args) else {
            return Ok(None);
        };
        let Some(gate) = self.runtime.egress_gate().filter(|_| plan.is_remote()) else {
            return Ok(None);
        };
        let scope = MediaScope::new(ctx.app_name(), ctx.user_id(), ctx.session_id());
        let cx =
            VisionCx::current().with_media_scope(scope.clone()).with_acting(ctx.acting_context());
        let artifacts = ctx.artifacts();
        let mut staged = StagedCall { digests: Vec::new(), evidence: Vec::new() };
        for image in &plan.images {
            let source = match self.resolve(artifacts.as_ref(), &scope, image).await {
                Ok(source) => source,
                // The tool reports an image it cannot read when it runs.
                Err(_) => return Ok(None),
            };
            let mut request = ReleaseRequest::new(&source, &plan.descriptor, plan.class.clone());
            if let Some(crop) = plan.crop {
                request = request.with_crop(crop);
            }
            match gate.stage(&cx, &request).await {
                Ok(stage) => {
                    staged.digests.push(stage.digest());
                    if let Some(evidence) = stage.image_evidence() {
                        staged.evidence.push(evidence);
                    }
                }
                Err(err) => {
                    let text = format!("`{}` was not run: {err}", kind.name());
                    return Ok(Some(Content::new("function").with_text(text)));
                }
            }
        }
        tracing::debug!(
            vision.tool = kind.name(),
            vision.staged = staged.digests.len(),
            "egress staged for approval"
        );
        lock(&self.staged).insert(key, staged);
        Ok(None)
    }

    /// The evidence staged for a call with these arguments.
    fn evidence_for(&self, kind: VisionToolKind, args: &Value) -> Vec<adk_core::Evidence> {
        lock(&self.staged)
            .get(&call_key(kind, args))
            .map(|staged| staged.evidence.iter().cloned().map(adk_core::Evidence::from).collect())
            .unwrap_or_default()
    }

    /// The egress digests this call may release, by the approval mode.
    fn approvals(
        &self,
        kind: VisionToolKind,
        plan: &Plan,
        call_id: &str,
        args: &Value,
    ) -> Result<Vec<Digest>> {
        let governed = lock(&self.governed).remove(&governed_key(call_id, kind, args));
        let staged = lock(&self.staged).remove(&call_key(kind, args));
        let Some((action, hosts)) = self.egress_action(plan) else {
            return Ok(Vec::new());
        };
        if action != EgressAction::Ask {
            return Ok(Vec::new());
        }
        match self.approval {
            EgressApproval::Explicit => Ok(lock(&self.approved).iter().copied().collect()),
            EgressApproval::GovernedPath => {
                let refused = |reason: &str| VisionError::EgressRefused {
                    host: hosts.join(", "),
                    reason: reason.to_string(),
                };
                if governed != Some(EgressAction::Ask) {
                    return Err(refused(
                        "the egress rule asks a person first and this call was not held for \
                         approval; install VisionToolset::egress_policy on the run",
                    ));
                }
                let Some(staged) = staged.filter(|staged| !staged.evidence.is_empty()) else {
                    return Err(refused(
                        "the call was held without the image it would send; install \
                         VisionToolset::evidence_callback on the agent and give the egress gate \
                         a media store",
                    ));
                };
                Ok(staged.digests)
            }
        }
    }

    /// The context a tool call runs vision calls under: the run's metering scope, the
    /// session's media scope, the acting agent and the run's audit trail.
    fn cx_for(ctx: &dyn ToolContext) -> VisionCx {
        let scope =
            ModelCallScope::current().or_else(|| ModelCallScope::for_tool(ctx)).unwrap_or_default();
        let acting = ctx.acting_context();
        let mut cx = VisionCx::new()
            .with_scope(scope)
            .with_media_scope(MediaScope::new(ctx.app_name(), ctx.user_id(), ctx.session_id()))
            .with_acting(acting.clone());
        if let Some(trail) = ctx.run_config().and_then(|config| config.audit.clone()) {
            cx = cx.with_audit(trail, AuditActor::new(&acting, ctx.invocation_id()));
        }
        cx
    }
}

/// The key a call's staged evidence is kept under: the tool and its canonical arguments.
fn call_key(kind: VisionToolKind, args: &Value) -> String {
    format!("{}\u{1f}{}", kind.name(), canonical_json(args))
}

/// The key the egress policy marks a call under: its function call ID and [`call_key`].
fn governed_key(call_id: &str, kind: VisionToolKind, args: &Value) -> String {
    format!("{call_id}\u{1e}{}", call_key(kind, args))
}

/// JSON with every object's keys sorted, so equal arguments give equal keys.
fn canonical_json(value: &Value) -> String {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut entries: Vec<(&String, &Value)> = map.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                Value::Object(entries.into_iter().map(|(k, v)| (k.clone(), sorted(v))).collect())
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    sorted(value).to_string()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
