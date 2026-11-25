//! The vision pack (spec 3.7 X15, feature `vision`).
//!
//! `adk-vision`'s `VisionToolset` does the work; the pack is classes, egress rules and the
//! privacy guardrail. Out of the box an image leaves as the agent sends it, unredacted and
//! unasked; each restriction below is an opt-in, enforced once chosen:
//!
//! | Piece | What the pack does |
//! |-------|--------------------|
//! | Tools | The tools the runtime has a backend for join the harness tools, so RBAC, hooks and the class registry cover them (table below) |
//! | Classes | A call that stays on the device is `Read`. A call that sends an image off it is `External`: trusted read-only under an `allow` rule, the default, otherwise held in Ask and Auto. ReadOnly and Plan deny every remote call. `vision_watch` lists as `Read`; starting or stopping a watch is `External`, `Idempotent` from a trusted origin |
//! | Egress rules | `.adk/vision.toml` in the workspace as written, or the operator's [`VisionPack::config`]; with neither, every release is allowed. [`VisionPack::distrust_workspace_rules`] makes a workspace file's `allow` rules ask |
//! | Redactors | Only for the classes the rules' `redact` list names: the operator's [`VisionPack::redactor`]s and, with feature `vision-local`, YuNet and RF-DETR Nano for `faces` and PP-OCRv6 for `personal_text` where none covers a listed class. A listed class no redactor covers refuses every release |
//! | Approvals | The toolset's egress policy joins the operator policy, so a rule that asks holds the call, and the held call carries the exact egress image as evidence |
//! | Privacy guardrail | Every image in a request to a remote harness model is released through the same gate or replaced by a placeholder; no agent tool removes it |
//! | Text | What `vision_read` and `vision_describe` return is screened and delivered inside a delimited untrusted block, as fetched text is (spec 3.7 V-I16) |
//!
//! Nothing in the pack is `ComputerUse`: screen actions stay in the computer-use and browser
//! packs.
//!
//! | Tool | Offered when | Does | Effect |
//! |------|--------------|------|--------|
//! | `vision_detect` | the runtime has a detector | Objects by class or concept, boxes in the image's pixels | `ReadOnly` |
//! | `vision_segment` | a segmenter | Masks from concepts, boxes or points | `ReadOnly` |
//! | `vision_read` | a document reader | Text, tables and fields, delivered as screened untrusted text | `ReadOnly` |
//! | `vision_describe` | a describer | An answer as text or a schema's value, delivered as screened untrusted text | `ReadOnly` |
//! | `vision_crop` | an editor | A new image of one region | `Idempotent` |
//! | `vision_redact` | an editor | A new image with regions filled solid black | `Idempotent` |
//! | `vision_annotate` | an editor | A new image with labelled boxes | `Idempotent` |
//! | `vision_measure` | a segmenter | Maximum length and area in millimetres under a fiducial's calibration | `ReadOnly` |
//! | `vision_watch` | [`VisionPack::frame_sources`] | Lists the operator's frame sources; starts or stops a watch condition that wakes a sleeping agent | `Idempotent` |
//!
//! A woken turn names the kept frame by the digest its wake payload carries; the image tools
//! load it from the session's artifacts, so the operator keeps wake frames in the session the
//! sleeping process runs in.

use std::path::Path;
use std::sync::Arc;

use adk_core::audit_log::AuditTrail;
use adk_core::egress::EgressPolicy;
use adk_core::{AdkError, Llm, Result, Tool, ToolContext, ToolEffect, ToolPolicy};
use adk_vision::stream::FrameSources;
use adk_vision::{
    EgressAction, EgressGate, EgressRecorder, ImagePrivacyGuardrail, InMemoryEgressRecorder,
    MediaStore, RedactionClass, Redactor, VISION_WATCH_TOOL, VisionConfig, VisionRuntime,
    VisionToolset,
};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::mode::HarnessMode;
use crate::policy::{CallClasses, ToolKind};

/// The workspace file the egress rules are read from.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::packs::vision::VISION_CONFIG_FILE, ".adk/vision.toml");
/// ```
pub const VISION_CONFIG_FILE: &str = ".adk/vision.toml";

/// Builds the runtime from the pack's egress gate and media store.
type RuntimeFactory =
    Box<dyn FnOnce(Arc<EgressGate>, MediaStore) -> adk_vision::Result<VisionRuntime> + Send>;

/// The vision pack: the governed vision tools under the harness policy, with the egress
/// rules and the privacy guardrail (spec 3.7 X15).
///
/// The application names its backends and, optionally, its redactors; the pack builds the
/// egress gate from the rules, hands it and the media store to the runtime factory, and
/// wires the rest.
///
/// # Default redactors
///
/// Nothing is redacted unless the rules' `redact` list names a class. For `faces` or
/// `personal_text` listed there and covered by no [`redactor`](Self::redactor), the pack
/// installs a default:
///
/// | Class | Feature `vision-local` | Without it |
/// |-------|------------------------|------------|
/// | `faces` | `FaceRedactor::on_device`: YuNet covers each face, RF-DETR Nano each person it finds no face for | No redactor: every release is refused |
/// | `personal_text` | `PersonalTextRedactor::pp_ocr` with PP-OCRv6 small and `PERSONAL_FIELD_LABELS` | No redactor: every release is refused |
///
/// The default backends load on their first release through the pack's `model_fetcher`
/// (feature `vision-local`), each file verified against adk-vision's built-in manifest. A
/// load that fails — `HF_HUB_OFFLINE` with the weights not cached, a digest mismatch —
/// refuses that release, except that the faces redactor covers whole persons when only
/// YuNet is missing. Either way an image the rules say to redact does not leave
/// unredacted; perception on the device is unaffected.
///
/// # The on-device profile
///
/// With `on_device = true` in the `[egress]` table of `.adk/vision.toml`, the egress gate
/// refuses every release at its destination step, with its reason, and the default fetcher
/// works offline, as under `HF_HUB_OFFLINE=1`: models load from the cache and no socket is
/// opened for them.
///
/// # Example
///
/// ```rust,ignore
/// use adk_vision::{MediaStore, PersonRedactor, PersonalTextRedactor, VisionRuntime};
/// use adk_harness::packs::vision::VisionPack;
///
/// let pack = VisionPack::new(MediaStore::new(artifacts), move |gate, store| {
///     VisionRuntime::builder()
///         .reader(local_ocr.clone())
///         .remote_describer(claude)
///         .shared_egress_gate(gate)
///         .media_store(store)
///         .build()
/// })
/// .redactor(Arc::new(PersonRedactor::new(rf_detr)))
/// .redactor(Arc::new(PersonalTextRedactor::new(local_ocr)))
/// .model_hosts(["api.anthropic.com"]);
/// let harness = HarnessAgent::builder("desk").model(model).workspace(ws).pack_vision(pack).build()?;
/// ```
pub struct VisionPack {
    store: MediaStore,
    factory: RuntimeFactory,
    redactors: Vec<Arc<dyn Redactor>>,
    config: Option<VisionConfig>,
    distrust_workspace_rules: bool,
    egress: EgressPolicy,
    recorder: Arc<dyn EgressRecorder>,
    model_hosts: Vec<String>,
    classes: (String, String),
    sources: Option<FrameSources>,
    #[cfg(feature = "vision-local")]
    fetcher: Option<adk_vision::ModelFetcher>,
}

impl std::fmt::Debug for VisionPack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VisionPack")
            .field("redactors", &self.redactors.iter().map(|r| r.class()).collect::<Vec<_>>())
            .field("config", &self.config)
            .field("model_hosts", &self.model_hosts)
            .finish_non_exhaustive()
    }
}

impl VisionPack {
    /// A pack whose images live in `store` and whose runtime `factory` builds from the
    /// pack's egress gate (`VisionRuntimeBuilder::shared_egress_gate`) and that store.
    pub fn new(
        store: MediaStore,
        factory: impl FnOnce(Arc<EgressGate>, MediaStore) -> adk_vision::Result<VisionRuntime>
        + Send
        + 'static,
    ) -> Self {
        Self {
            store,
            factory: Box::new(factory),
            redactors: Vec::new(),
            config: None,
            distrust_workspace_rules: false,
            egress: EgressPolicy::allow_private_networks(),
            recorder: Arc::new(InMemoryEgressRecorder::new()),
            model_hosts: Vec::new(),
            classes: ("document".to_string(), "document_crop".to_string()),
            sources: None,
            #[cfg(feature = "vision-local")]
            fetcher: None,
        }
    }

    /// Offers `vision_watch` over `sources`: the agent lists the operator's frame sources and
    /// starts or stops watch conditions on them; each event keeps its frame and wakes the
    /// process sleeping on the source (spec 3.7 §7.8, §7.10).
    pub fn frame_sources(mut self, sources: FrameSources) -> Self {
        self.sources = Some(sources);
        self
    }

    /// Installs an on-device redactor: `PersonRedactor` for faces, `PersonalTextRedactor`
    /// for personal text. A class covered here gets no default redactor; a class the rules
    /// require that nothing covers refuses every release.
    pub fn redactor(mut self, redactor: Arc<dyn Redactor>) -> Self {
        self.redactors.push(redactor);
        self
    }

    /// Fetches the default redactors' weights through `fetcher` instead of one built from
    /// the environment (`HF_HUB_CACHE`, `HF_HUB_OFFLINE`, `HF_TOKEN`).
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use adk_vision::{FetchConfig, InMemoryAcceptanceStore, ModelFetcher};
    ///
    /// let config = FetchConfig::from_env().with_cache_dir("/var/lib/adk/models").with_offline(true);
    /// let fetcher = ModelFetcher::new(config, Arc::new(InMemoryAcceptanceStore::new()));
    /// # let _ = fetcher;
    /// // VisionPack::new(store, factory).model_fetcher(fetcher)
    /// ```
    #[cfg(feature = "vision-local")]
    pub fn model_fetcher(mut self, fetcher: adk_vision::ModelFetcher) -> Self {
        self.fetcher = Some(fetcher);
        self
    }

    /// Uses `config` as given instead of reading `.adk/vision.toml`.
    pub fn config(mut self, config: VisionConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Treats the workspace's `.adk/vision.toml` as untrusted: its `allow` rules and an
    /// `allow` default ask instead, so a file anyone can commit can tighten the rules but
    /// never release an image unasked. Without this, the file's rules apply as written.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let pack = VisionPack::new(store, factory).distrust_workspace_rules();
    /// ```
    pub fn distrust_workspace_rules(mut self) -> Self {
        self.distrust_workspace_rules = true;
        self
    }

    /// The destination policy the gate checks each backend host against: any host, on a
    /// public or private network, by default (never a cloud metadata endpoint).
    /// `EgressPolicy::public_only()`, an allowlist, or `private_networks = false` in the
    /// rules restrict it.
    pub fn egress_policy(mut self, policy: EgressPolicy) -> Self {
        self.egress = policy;
        self
    }

    /// Where the gate writes each release and refusal, besides the run's audit trail.
    pub fn recorder(mut self, recorder: Arc<dyn EgressRecorder>) -> Self {
        self.recorder = recorder;
        self
    }

    /// The hosts the harness model sends requests to, for the privacy guardrail's egress
    /// rules and records, in place of the hosts the model names
    /// (`Llm::data_destinations`); the guardrail guards a remote model either way.
    pub fn model_hosts(mut self, hosts: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.model_hosts = hosts.into_iter().map(Into::into).collect();
        self
    }

    /// The data classes the tools release images as: `document` and `document_crop` by
    /// default.
    pub fn data_classes(mut self, image: &str, crop: &str) -> Self {
        self.classes = (image.to_string(), crop.to_string());
        self
    }

    /// Builds the gate, the runtime and the toolset for a harness over `root`.
    pub(crate) fn into_runtime(self, root: &Path) -> Result<VisionPackRuntime> {
        let config = match self.config {
            Some(config) => config,
            None => load_vision_config(root, self.distrust_workspace_rules)?,
        };
        let uncovered: Vec<RedactionClass> =
            [RedactionClass::faces(), RedactionClass::personal_text()]
                .into_iter()
                .filter(|class| config.egress.redact.contains(class))
                .filter(|class| !self.redactors.iter().any(|r| &r.class() == class))
                .collect();
        #[cfg(feature = "vision-local")]
        let defaults: Vec<Arc<dyn Redactor>> = {
            let on_device = config.egress.on_device;
            let fetcher = self.fetcher.unwrap_or_else(|| {
                let fetch = adk_vision::FetchConfig::from_env();
                adk_vision::ModelFetcher::new(
                    if on_device { fetch.with_offline(true) } else { fetch },
                    Arc::new(adk_vision::InMemoryAcceptanceStore::new()),
                )
            });
            uncovered
                .into_iter()
                .map(|class| -> Arc<dyn Redactor> {
                    tracing::debug!(vision.redaction_class = %class, "vision pack installs its default redactor");
                    if class == RedactionClass::faces() {
                        Arc::new(adk_vision::FaceRedactor::on_device(fetcher.clone()))
                    } else {
                        Arc::new(
                            adk_vision::PersonalTextRedactor::pp_ocr(
                                fetcher.clone(),
                                adk_vision::PpOcrV6Size::Small,
                            )
                            .with_labelled_fields(
                                adk_vision::PERSONAL_FIELD_LABELS.iter().copied(),
                            ),
                        )
                    }
                })
                .collect()
        };
        #[cfg(not(feature = "vision-local"))]
        let defaults: Vec<Arc<dyn Redactor>> = {
            for class in uncovered {
                tracing::warn!(
                    vision.redaction_class = %class,
                    "no redactor covers a class the egress rules require, so every release is \
                     refused; install one with VisionPack::redactor or enable adk-harness's \
                     `vision-local` feature"
                );
            }
            Vec::new()
        };
        let mut gate = EgressGate::new(self.egress, config.egress, self.recorder)
            .with_media_store(self.store.clone());
        for redactor in self.redactors.into_iter().chain(defaults) {
            gate = gate.with_redactor(redactor);
        }
        let gate = Arc::new(gate);
        let runtime = (self.factory)(gate.clone(), self.store).map_err(AdkError::from)?;
        if runtime.shared_egress_gate().is_none_or(|shared| !Arc::ptr_eq(&shared, &gate)) {
            return Err(AdkError::config(
                "the vision pack's runtime must release through the pack's egress gate: call \
                 VisionRuntimeBuilder::shared_egress_gate with the gate the factory is given \
                 (spec 3.7 X15)",
            ));
        }
        let mut toolset = VisionToolset::builder(Arc::new(runtime))
            .data_classes(&self.classes.0, &self.classes.1);
        if let Some(sources) = self.sources {
            toolset = toolset.frame_sources(sources);
        }
        let toolset = toolset.build().map_err(AdkError::from)?;
        Ok(VisionPackRuntime { toolset, gate, model_hosts: self.model_hosts })
    }
}

/// The egress rules of a workspace: `.adk/vision.toml` as written when it exists, else the
/// defaults, under which every release is allowed and nothing is redacted.
///
/// With `distrust_allow`, the file is treated as untrusted: its `allow` rules and an
/// `allow` default ask instead, and with no file every release asks
/// ([`VisionPack::distrust_workspace_rules`]).
///
/// # Errors
///
/// A configuration error naming the file when it cannot be read or parsed.
///
/// # Example
///
/// ```rust
/// use adk_harness::packs::vision::load_vision_config;
/// use adk_vision::{EgressAction, RedactionClass};
///
/// let dir = tempfile::tempdir().unwrap();
/// let open = load_vision_config(dir.path(), false).unwrap();
/// assert_eq!(open.egress.default, EgressAction::Allow);
/// assert!(open.egress.redact.is_empty());
///
/// std::fs::create_dir_all(dir.path().join(".adk")).unwrap();
/// std::fs::write(
///     dir.path().join(".adk/vision.toml"),
///     "[egress]\ndefault = \"allow\"\nredact = [\"faces\"]\n",
/// )
/// .unwrap();
/// let as_written = load_vision_config(dir.path(), false).unwrap();
/// assert_eq!(as_written.egress.default, EgressAction::Allow);
/// assert_eq!(as_written.egress.redact, [RedactionClass::faces()]);
///
/// let distrusted = load_vision_config(dir.path(), true).unwrap();
/// assert_eq!(distrusted.egress.default, EgressAction::Ask);
/// ```
pub fn load_vision_config(root: &Path, distrust_allow: bool) -> Result<VisionConfig> {
    let path = root.join(VISION_CONFIG_FILE);
    let mut config = match std::fs::read_to_string(&path) {
        Ok(text) => VisionConfig::from_toml_str(&text)
            .map_err(|err| AdkError::config(format!("{} is invalid: {err}", path.display())))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => VisionConfig::default(),
        Err(err) => {
            return Err(AdkError::config(format!("{} could not be read: {err}", path.display())));
        }
    };
    if distrust_allow {
        if config.egress.default == EgressAction::Allow {
            config.egress.default = EgressAction::Ask;
        }
        for rule in &mut config.egress.rules {
            if rule.action == EgressAction::Allow {
                rule.action = EgressAction::Ask;
            }
        }
    }
    Ok(config)
}

/// What the harness holds for the vision pack at run time.
pub(crate) struct VisionPackRuntime {
    pub(crate) toolset: VisionToolset,
    gate: Arc<EgressGate>,
    model_hosts: Vec<String>,
}

impl VisionPackRuntime {
    /// The tools, each screening the text it returns.
    pub(crate) fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.toolset
            .tools()
            .into_iter()
            .map(|tool| match tool.name() {
                "vision_read" | "vision_describe" => {
                    Arc::new(ScreenedVisionTool { inner: tool }) as Arc<dyn Tool>
                }
                _ => tool,
            })
            .collect()
    }

    /// The toolset's egress policy, composed into the operator policy.
    pub(crate) fn policy(&self) -> Arc<dyn ToolPolicy> {
        self.toolset.egress_policy()
    }

    /// The per-call classes of the vision tools.
    pub(crate) fn classes(&self) -> Arc<dyn CallClasses> {
        Arc::new(VisionClasses { toolset: self.toolset.clone() })
    }

    /// The privacy guardrail for the harness model, writing to the harness audit trail.
    pub(crate) fn guardrail(
        &self,
        model: &dyn Llm,
        audit: Option<AuditTrail>,
    ) -> Arc<ImagePrivacyGuardrail> {
        let mut hosts = self.model_hosts.clone();
        if hosts.is_empty() {
            hosts = adk_core::DataDestination::all_hosts(&model.data_destinations());
        }
        if hosts.is_empty() {
            hosts.push(model.name().to_string());
        }
        let guardrail = ImagePrivacyGuardrail::for_model(self.gate.clone(), model, hosts);
        Arc::new(match audit {
            Some(trail) => guardrail.with_audit(trail),
            None => guardrail,
        })
    }
}

/// Classes each vision call by where its image goes (spec 3.7 §7.8): `Read` on the device,
/// `External` off it, trusted read-only only under an `allow` rule.
///
/// # Example
///
/// ```rust
/// use adk_core::ToolEffect;
/// use adk_harness::ToolKind;
/// use adk_harness::packs::vision::class_for;
/// use adk_vision::EgressAction;
///
/// assert_eq!(class_for(None), ToolKind::Read);
/// assert_eq!(
///     class_for(Some(EgressAction::Allow)),
///     ToolKind::External { effect: ToolEffect::ReadOnly, trusted_origin: true }
/// );
/// assert_eq!(
///     class_for(Some(EgressAction::Ask)),
///     ToolKind::External { effect: ToolEffect::ReadOnly, trusted_origin: false }
/// );
/// ```
pub fn class_for(action: Option<EgressAction>) -> ToolKind {
    match action {
        None => ToolKind::Read,
        Some(EgressAction::Allow) => {
            ToolKind::External { effect: ToolEffect::ReadOnly, trusted_origin: true }
        }
        Some(EgressAction::Ask | EgressAction::Deny) => {
            ToolKind::External { effect: ToolEffect::ReadOnly, trusted_origin: false }
        }
    }
}

#[derive(Debug)]
struct VisionClasses {
    toolset: VisionToolset,
}

impl CallClasses for VisionClasses {
    fn class_of_call(&self, tool: &str, args: &Value) -> Option<ToolKind> {
        if !self.toolset.is_tool(tool) {
            return None;
        }
        if tool == VISION_WATCH_TOOL {
            return Some(watch_class(args));
        }
        Some(class_for(self.toolset.egress_action(tool, args)))
    }

    // ReadOnly and Plan are restrictions the person chose: nothing leaves the machine in
    // them, whatever the egress rules allow.
    fn denial_in_mode(&self, tool: &str, args: &Value, mode: HarnessMode) -> Option<String> {
        if !matches!(mode, HarnessMode::ReadOnly | HarnessMode::Plan)
            || tool == VISION_WATCH_TOOL
            || !self.toolset.is_tool(tool)
        {
            return None;
        }
        self.toolset.egress_action(tool, args).map(|_| {
            format!(
                "'{tool}' sends an image off this machine, which {} mode does not allow; switch \
                 to Ask or Auto, or use a backend on the device",
                mode.as_str()
            )
        })
    }
}

/// The class of a `vision_watch` call: `list` reads; `start` and `stop` change what streams
/// and what wakes, so they are `External`, `Idempotent` from the pack's trusted origin.
///
/// # Example
///
/// ```rust
/// use adk_core::ToolEffect;
/// use adk_harness::ToolKind;
/// use adk_harness::packs::vision::watch_class;
/// use serde_json::json;
///
/// assert_eq!(watch_class(&json!({ "action": "list" })), ToolKind::Read);
/// assert_eq!(
///     watch_class(&json!({ "action": "start", "source": "line-1", "name": "seal" })),
///     ToolKind::External { effect: ToolEffect::Idempotent, trusted_origin: true }
/// );
/// ```
pub fn watch_class(args: &Value) -> ToolKind {
    match args.get("action").and_then(Value::as_str) {
        Some("list") => ToolKind::Read,
        _ => ToolKind::External { effect: ToolEffect::Idempotent, trusted_origin: true },
    }
}

/// `vision_read` and `vision_describe` with their recognised text screened and delivered
/// inside a delimited untrusted block: text an image shows is data, never instructions
/// (spec 3.7 V-I16).
struct ScreenedVisionTool {
    inner: Arc<dyn Tool>,
}

#[async_trait]
impl Tool for ScreenedVisionTool {
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

    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }

    fn effect(&self) -> ToolEffect {
        self.inner.effect()
    }

    fn egress(&self, args: &Value) -> adk_core::egress::ToolEgress {
        self.inner.egress(args)
    }

    fn approval_evidence(&self, args: &Value) -> Vec<adk_core::Evidence> {
        self.inner.approval_evidence(args)
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let mut result = self.inner.execute(ctx, args).await?;
        let image = result.get("image").and_then(Value::as_str).unwrap_or_default().to_string();
        let mut lines = Vec::new();
        if let Some(blocks) = result.get_mut("blocks").and_then(Value::as_array_mut) {
            for (index, block) in blocks.iter_mut().enumerate() {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    lines.push(format!("[{}] {text}", index + 1));
                }
                if let Value::Object(block) = block {
                    block.remove("text");
                    block.insert("line".into(), json!(index + 1));
                }
            }
        }
        if let Value::Object(map) = &mut result {
            if let Some(text) = map.remove("text").filter(|t| !t.is_null()) {
                lines.push(text.as_str().map_or_else(|| text.to_string(), str::to_string));
            }
            if let Some(value) = map.remove("value").filter(|v| !v.is_null()) {
                lines.push(value.to_string());
            }
            if let Some(markdown) = map.remove("markdown").filter(|m| !m.is_null()) {
                lines.push(markdown.as_str().unwrap_or_default().to_string());
            }
            let screened = crate::web::screen(&lines.join("\n")).await;
            let attributes = [("image", image.as_str()), ("screening", screened.screening())];
            map.insert(
                "content".into(),
                json!(crate::web::untrusted_block("vision", &attributes, &screened.text)),
            );
        }
        Ok(result)
    }
}
