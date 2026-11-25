//! [`ImagePrivacyGuardrail`] and [`PersonalTextRedactor`] (spec 3.7 §7.8, feature
//! `guardrail`).
//!
//! The egress gate guards what the vision tools send. The guardrail runs the same gate on
//! every other path an image could take to a remote model the framework controls:
//!
//! | Path | Hook | What happens to an image |
//! |------|------|--------------------------|
//! | Model calls | [`ImagePrivacyGuardrail::before_model_callback`] | Every image part in the request — user uploads, tool-result images — is released through the gate, or replaced by a text placeholder that says why |
//! | Tool results | [`ToolGuardrail::validate_result`] | A base64 image in a tool's JSON result, such as a screenshot, is replaced by its released copy or a placeholder |
//! | User input | [`Guardrail::validate`] | As for model calls, returned as `Transform` |
//!
//! An image the gate already released passes through: its part carries the release
//! annotation and its bytes hash to a digest the gate released. A model that runs on the
//! device (`Llm::data_locality()` is not `Remote`) is not guarded, since nothing leaves.
//! User code that calls an `Llm` directly, without the guardrail, is outside this guarantee.
//!
//! [`PersonalTextRedactor`] finds the personal text the gate covers: words a local
//! [`DocumentReader`] reads that `adk_guardrail::PiiRedactor`'s patterns match (email,
//! phone, SSN, card number), and the values of labelled fields such as `Name:`.

use std::sync::Arc;

use adk_core::{
    BeforeModelCallback, BeforeModelResult, CallbackContext, Content, DataLocality, Llm, Part,
};
use adk_guardrail::{
    Guardrail, GuardrailResult, PiiRedactor, ToolGuardrail, ToolGuardrailResult,
    ToolResultGuardrailResult,
};
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;

use crate::backend::{BackendDescriptor, Locality};
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::egress::{DataClass, EgressGate, RedactionClass, Redactor, ReleaseRequest};
use crate::error::Result;
use crate::geometry::{BBox, CoordFrame, RawRegion};
use crate::media::{DecodeLimits, MediaScope, SourceImage};
use crate::redact::{OnDevice, fit_on_device, to_image_boxes};
use crate::sent::SentImage;
use crate::traits::{Block, DocumentReader, ReadOptions};

/// The data class images bound for a remote model's context are released as.
pub const MODEL_CONTEXT_CLASS: &str = "model_context";

/// Runs the egress gate on every image the framework sends to a remote model.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_core::egress::EgressPolicy;
/// use adk_vision::{
///     EgressConfig, EgressGate, ImagePrivacyGuardrail, InMemoryEgressRecorder,
/// };
///
/// let gate = Arc::new(EgressGate::new(
///     EgressPolicy::allow_private_networks(),
///     EgressConfig::default(),
///     Arc::new(InMemoryEgressRecorder::new()),
/// ));
/// let guardrail = ImagePrivacyGuardrail::new(gate, "claude", ["api.anthropic.com"]);
/// assert!(guardrail.guards());
/// ```
pub struct ImagePrivacyGuardrail {
    gate: Arc<EgressGate>,
    destination: BackendDescriptor,
    data_class: DataClass,
    locality: DataLocality,
    limits: DecodeLimits,
    audit: Option<adk_core::audit_log::AuditTrail>,
}

impl std::fmt::Debug for ImagePrivacyGuardrail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImagePrivacyGuardrail")
            .field("destination", &self.destination.locality)
            .field("data_class", &self.data_class)
            .field("locality", &self.locality)
            .finish_non_exhaustive()
    }
}

/// What gating one content changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct GateOutcome {
    /// Egress digests of the images released.
    pub released: Vec<Digest>,
    /// Source digests of the images withheld, with why.
    pub withheld: Vec<(Option<Digest>, String)>,
}

impl GateOutcome {
    /// Whether any image was released or withheld.
    pub fn changed(&self) -> bool {
        !self.released.is_empty() || !self.withheld.is_empty()
    }
}

impl ImagePrivacyGuardrail {
    /// A guardrail releasing images through `gate` to a remote model named `model` at
    /// `hosts`, as data class `model_context`.
    pub fn new(
        gate: Arc<EgressGate>,
        model: impl Into<String>,
        hosts: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let hosts = hosts.into_iter().map(Into::into).collect();
        Self {
            gate,
            destination: BackendDescriptor::new(
                model,
                Locality::Remote { hosts },
                CoordFrame::SentPixels,
            ),
            data_class: DataClass::new(MODEL_CONTEXT_CLASS),
            locality: DataLocality::Remote,
            limits: DecodeLimits::default(),
            audit: None,
        }
    }

    /// A guardrail for `model`, guarding only when its `data_locality()` is `Remote`.
    ///
    /// Images are released to `hosts`, or with none to the hosts the model names
    /// ([`Llm::data_destinations`]); `provider` egress rules match the providers it names.
    pub fn for_model(
        gate: Arc<EgressGate>,
        model: &dyn Llm,
        hosts: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let destinations = model.data_destinations();
        let mut hosts: Vec<String> = hosts.into_iter().map(Into::into).collect();
        if hosts.is_empty() {
            hosts = adk_core::DataDestination::all_hosts(&destinations);
        }
        let mut guardrail = Self::new(gate, model.name(), hosts);
        guardrail.locality = model.data_locality();
        for destination in destinations {
            guardrail.destination = guardrail.destination.with_provider(destination.provider);
        }
        guardrail
    }

    /// Releases images as `class` instead of `model_context`.
    pub fn with_data_class(mut self, class: &str) -> Self {
        self.data_class = DataClass::new(class);
        self
    }

    /// Writes the `vision.egress.*` records of model-context releases to `trail` too.
    pub fn with_audit(mut self, trail: adk_core::audit_log::AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// Whether the guarded model's requests leave the machine.
    pub fn guards(&self) -> bool {
        self.locality.leaves_machine()
    }

    /// Releases or withholds every image part of `content`, in place.
    pub async fn gate_content(&self, cx: &VisionCx, content: &mut Content) -> GateOutcome {
        let mut outcome = GateOutcome::default();
        if !self.guards() {
            return outcome;
        }
        for part in &mut content.parts {
            match part {
                Part::InlineData { mime_type, data, annotations, .. }
                    if mime_type.starts_with("image/") =>
                {
                    if self.already_released(data, annotations.as_ref()) {
                        continue;
                    }
                    *part = match self.release_bytes(cx, data).await {
                        Ok(released) => {
                            outcome.released.push(released.receipt().egress);
                            released.to_part()
                        }
                        Err((source, reason)) => {
                            outcome.withheld.push((source, reason.clone()));
                            Part::Text { text: placeholder(source, &reason) }
                        }
                    };
                }
                Part::FunctionResponse { function_response, .. } => {
                    let mut kept = Vec::with_capacity(function_response.inline_data.len());
                    for inline in std::mem::take(&mut function_response.inline_data) {
                        if !inline.mime_type.starts_with("image/")
                            || self.already_released(&inline.data, inline.annotations.as_ref())
                        {
                            kept.push(inline);
                            continue;
                        }
                        match self.release_bytes(cx, &inline.data).await {
                            Ok(released) => {
                                outcome.released.push(released.receipt().egress);
                                let mut inline = inline;
                                inline.data = released.bytes().to_vec();
                                inline.mime_type = released.mime_type().to_string();
                                kept.push(inline);
                            }
                            Err((source, reason)) => {
                                note_withheld(&mut function_response.response, source, &reason);
                                outcome.withheld.push((source, reason));
                            }
                        }
                    }
                    function_response.inline_data = kept;
                }
                _ => {}
            }
        }
        outcome
    }

    /// The `BeforeModelCallback` that gates every image in a request to the guarded model.
    pub fn before_model_callback(self: &Arc<Self>) -> BeforeModelCallback {
        let guardrail = Arc::clone(self);
        Box::new(move |ctx: Arc<dyn CallbackContext>, mut request: adk_core::LlmRequest| {
            let guardrail = Arc::clone(&guardrail);
            Box::pin(async move {
                if !guardrail.guards() {
                    return Ok(BeforeModelResult::Continue(request));
                }
                let acting = ctx.acting_context();
                let mut cx = VisionCx::current()
                    .with_media_scope(MediaScope::new(
                        ctx.app_name(),
                        ctx.user_id(),
                        ctx.session_id(),
                    ))
                    .with_acting(acting.clone());
                if let Some(trail) = &guardrail.audit {
                    let actor = adk_core::audit_log::AuditActor::new(&acting, ctx.invocation_id());
                    cx = cx.with_audit(trail.clone(), actor);
                }
                for content in &mut request.contents {
                    guardrail.gate_content(&cx, content).await;
                }
                Ok(BeforeModelResult::Continue(request))
            })
        })
    }

    fn already_released(&self, data: &[u8], annotations: Option<&Value>) -> bool {
        let released = annotations
            .and_then(|a| a.get("adkVision"))
            .and_then(|v| v.get("release"))
            .is_some_and(|release| !release.is_null());
        released && self.gate.has_released(&Digest::of(data))
    }

    async fn release_bytes(
        &self,
        cx: &VisionCx,
        data: &[u8],
    ) -> std::result::Result<crate::egress::ReleasedImage, (Option<Digest>, String)> {
        let source = SourceImage::decode(data, &self.limits)
            .map_err(|err| (None, format!("it could not be read on this computer: {err}")))?;
        let request = ReleaseRequest::new(&source, &self.destination, self.data_class.clone());
        self.gate.release(cx, request).await.map_err(|err| (Some(source.digest()), err.to_string()))
    }

    /// Releases or withholds the images inside a JSON tool result.
    async fn gate_value(&self, cx: &VisionCx, value: &mut Value, outcome: &mut GateOutcome) {
        match value {
            Value::String(text) => {
                let Some((prefix, bytes)) = embedded_image(text) else {
                    return;
                };
                *text = match self.release_bytes(cx, &bytes).await {
                    Ok(released) => {
                        outcome.released.push(released.receipt().egress);
                        let encoded = STANDARD.encode(released.bytes());
                        match prefix {
                            Some(_) => format!("data:image/png;base64,{encoded}"),
                            None => encoded,
                        }
                    }
                    Err((source, reason)) => {
                        outcome.withheld.push((source, reason.clone()));
                        placeholder(source, &reason)
                    }
                };
            }
            Value::Array(items) => {
                for item in items {
                    Box::pin(self.gate_value(cx, item, outcome)).await;
                }
            }
            Value::Object(map) => {
                for (_, item) in map.iter_mut() {
                    Box::pin(self.gate_value(cx, item, outcome)).await;
                }
            }
            _ => {}
        }
    }
}

/// The text a withheld image is replaced by.
fn placeholder(source: Option<Digest>, reason: &str) -> String {
    match source {
        Some(digest) => format!(
            "[image {digest} withheld from this model: {reason}. The vision tools can read it on this computer.]"
        ),
        None => format!("[image withheld from this model: {reason}]"),
    }
}

fn note_withheld(response: &mut Value, source: Option<Digest>, reason: &str) {
    let note = Value::String(placeholder(source, reason));
    match response {
        Value::Object(map) => {
            let withheld = map.entry("withheldImages").or_insert_with(|| Value::Array(Vec::new()));
            if let Value::Array(items) = withheld {
                items.push(note);
            }
        }
        other => {
            *other = serde_json::json!({ "result": other.take(), "withheldImages": [note] });
        }
    }
}

/// A base64 image inside a string: a `data:image/...;base64,` URL or bare base64 that
/// starts with a PNG, JPEG, WebP or GIF signature. Returns the data-URL prefix, if any, and
/// the decoded bytes.
fn embedded_image(text: &str) -> Option<(Option<&str>, Vec<u8>)> {
    let (prefix, payload) = match text.strip_prefix("data:image/") {
        Some(rest) => {
            let (_, payload) = rest.split_once(";base64,")?;
            (Some(&text[..text.len() - payload.len()]), payload)
        }
        None => {
            // A bare payload: PNG, JPEG, WebP (RIFF) and GIF signatures in base64.
            let signature = ["iVBORw0KGgo", "/9j/", "UklGR", "R0lGOD"];
            if text.len() < 64 || !signature.iter().any(|s| text.starts_with(s)) {
                return None;
            }
            (None, text)
        }
    };
    let bytes = STANDARD.decode(payload.trim()).ok()?;
    Some((prefix, bytes))
}

#[async_trait]
impl Guardrail for ImagePrivacyGuardrail {
    fn name(&self) -> &str {
        "image_privacy"
    }

    async fn validate(&self, content: &Content) -> GuardrailResult {
        let mut gated = content.clone();
        let outcome = self.gate_content(&VisionCx::new(), &mut gated).await;
        if !outcome.changed() {
            return GuardrailResult::Pass;
        }
        GuardrailResult::Transform {
            new_content: gated,
            reason: format!(
                "{} image(s) released after redaction, {} withheld",
                outcome.released.len(),
                outcome.withheld.len()
            ),
        }
    }

    fn run_parallel(&self) -> bool {
        false
    }
}

#[async_trait]
impl ToolGuardrail for ImagePrivacyGuardrail {
    fn name(&self) -> &str {
        "image_privacy"
    }

    async fn validate_call(&self, _tool: &str, _args: &Value) -> ToolGuardrailResult {
        ToolGuardrailResult::Allow
    }

    async fn validate_result(
        &self,
        _tool: &str,
        _args: &Value,
        result: &Value,
    ) -> ToolResultGuardrailResult {
        if !self.guards() {
            return ToolResultGuardrailResult::Allow;
        }
        let mut gated = result.clone();
        let mut outcome = GateOutcome::default();
        self.gate_value(&VisionCx::new(), &mut gated, &mut outcome).await;
        if !outcome.changed() {
            return ToolResultGuardrailResult::Allow;
        }
        ToolResultGuardrailResult::revise(
            gated,
            format!(
                "{} image(s) released after redaction, {} withheld",
                outcome.released.len(),
                outcome.withheld.len()
            ),
        )
    }
}

/// The field labels whose values identify a person on identity documents and forms:
/// `PersonalTextRedactor::with_labelled_fields` takes them as the harness vision pack's
/// default.
///
/// # Example
///
/// ```
/// assert!(adk_vision::PERSONAL_FIELD_LABELS.contains(&"Date of birth"));
/// ```
pub const PERSONAL_FIELD_LABELS: &[&str] = &[
    "Name",
    "Full name",
    "Surname",
    "Given names",
    "Date of birth",
    "Place of birth",
    "Address",
    "Email",
    "Phone",
];

/// Finds personal text: words an on-device [`DocumentReader`] reads that
/// `adk_guardrail::PiiRedactor`'s patterns match, and the values of labelled fields.
///
/// The reader's words are joined per block with single spaces, `PiiRedactor::find` returns
/// byte ranges in that text (spec 3.7 X18), and every word a match touches is covered. A
/// labelled field — a block whose words start with one of the labels, such as `Name` or
/// `Date of birth`, with or without a colon — has every word after the label covered; when
/// the reader returns the label as a block of its own, the nearest block to its right on
/// the same line is the value and is covered whole. A block with no words is covered whole
/// when its text matches.
///
/// `PersonalTextRedactor::pp_ocr` (feature `ocr`) is the default reader: PP-OCRv6 from the
/// built-in manifest, loaded on first use.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_vision::{PersonalTextRedactor, RedactionClass, Redactor};
/// # use adk_vision::{BackendDescriptor, CoordFrame, DocumentPage, DocumentReader, Locality, RawRegion, ReadOptions, SentImage, VisionCx};
/// # struct Blank(BackendDescriptor);
/// # #[async_trait::async_trait]
/// # impl DocumentReader for Blank {
/// #     fn descriptor(&self) -> &BackendDescriptor { &self.0 }
/// #     async fn read(&self, _: &VisionCx, _: &SentImage, _: &ReadOptions) -> adk_vision::Result<DocumentPage<RawRegion>> { Ok(DocumentPage::default()) }
/// # }
/// # let reader = Arc::new(Blank(BackendDescriptor::new("ocr", Locality::InProcess, CoordFrame::SentPixels)));
///
/// let redactor = PersonalTextRedactor::new(reader).with_labelled_fields(["Name", "Date of birth"]);
/// assert_eq!(redactor.class(), RedactionClass::personal_text());
/// ```
pub struct PersonalTextRedactor {
    reader: OnDevice<dyn DocumentReader>,
    pii: PiiRedactor,
    labels: Vec<Vec<String>>,
}

impl std::fmt::Debug for PersonalTextRedactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersonalTextRedactor")
            .field("reader", &self.reader.name())
            .field("labels", &self.labels)
            .finish_non_exhaustive()
    }
}

impl PersonalTextRedactor {
    /// Covers the words `reader` reads that `PiiRedactor::new()`'s patterns match.
    pub fn new(reader: Arc<dyn DocumentReader>) -> Self {
        let name = reader.descriptor().id.to_string();
        Self { reader: OnDevice::ready(name, reader), pii: PiiRedactor::new(), labels: Vec::new() }
    }

    /// Covers the words PP-OCRv6 of `size` reads that `PiiRedactor::new()`'s patterns match:
    /// the built-in `pp-ocrv6-{size}-det` and `-rec` manifests, fetched through `fetcher`
    /// and verified against their SHA-256 on the redactor's first use (feature `ocr`).
    ///
    /// Nothing loads here. When the weights cannot be had — `HF_HUB_OFFLINE` with an empty
    /// cache, a digest mismatch — `find` fails, and the egress gate refuses the release.
    ///
    /// # Example
    ///
    /// ```
    /// use std::sync::Arc;
    /// use adk_vision::{
    ///     FetchConfig, InMemoryAcceptanceStore, ModelFetcher, PERSONAL_FIELD_LABELS,
    ///     PersonalTextRedactor, PpOcrV6Size, RedactionClass, Redactor,
    /// };
    ///
    /// let fetcher =
    ///     ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    /// let redactor = PersonalTextRedactor::pp_ocr(fetcher, PpOcrV6Size::Small)
    ///     .with_labelled_fields(PERSONAL_FIELD_LABELS.iter().copied());
    /// assert_eq!(redactor.class(), RedactionClass::personal_text());
    /// ```
    #[cfg(feature = "ocr")]
    pub fn pp_ocr(fetcher: crate::ModelFetcher, size: crate::PpOcrV6Size) -> Self {
        use crate::error::VisionError;
        use crate::redact::{Load, Loading};

        let name = size.recognition_manifest().trim_end_matches("-rec").to_string();
        let load: Load<dyn DocumentReader> = Box::new(move || {
            let fetcher = fetcher.clone();
            Box::pin(async move {
                let manifests = crate::ManifestSet::builtin()?;
                let lookup = |id: String| {
                    manifests.get(&id).cloned().ok_or_else(|| VisionError::Manifest {
                        id,
                        reason: "not in the built-in manifest".into(),
                    })
                };
                let det = fetcher.fetch(&lookup(size.detection_manifest())?).await?;
                let rec = fetcher.fetch(&lookup(size.recognition_manifest())?).await?;
                let reader = tokio::task::spawn_blocking(move || {
                    crate::PpOcrReader::from_models(size, &det, &rec)
                })
                .await
                .map_err(|e| VisionError::Backend {
                    backend: "pp-ocrv6".into(),
                    message: format!("the load task failed: {e}"),
                })??;
                tracing::info!(vision.backend = %reader.descriptor().id, "personal text redactor loaded");
                Ok(Arc::new(reader) as Arc<dyn DocumentReader>)
            }) as Loading<dyn DocumentReader>
        });
        Self { reader: OnDevice::lazy(name, load), pii: PiiRedactor::new(), labels: Vec::new() }
    }

    /// Matches with `pii`'s patterns instead.
    pub fn with_pii(mut self, pii: PiiRedactor) -> Self {
        self.pii = pii;
        self
    }

    /// Also covers the value of every field labelled with one of `labels`, such as `Name`.
    pub fn with_labelled_fields(
        mut self,
        labels: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.labels = labels
            .into_iter()
            .map(|label| label.into().split_whitespace().map(normalize_word).collect())
            .filter(|words: &Vec<String>| !words.is_empty())
            .collect();
        self
    }

    /// The indices of the words of one block that are personal.
    fn personal_words(&self, words: &[String]) -> Vec<usize> {
        let mut text = String::new();
        let mut spans = Vec::with_capacity(words.len());
        for word in words {
            if !text.is_empty() {
                text.push(' ');
            }
            let start = text.len();
            text.push_str(word);
            spans.push(start..text.len());
        }
        let mut personal: Vec<usize> = Vec::new();
        for found in self.pii.find(&text) {
            for (index, span) in spans.iter().enumerate() {
                if span.start < found.range.end && found.range.start < span.end {
                    personal.push(index);
                }
            }
        }
        let normalized: Vec<String> = words.iter().map(normalize_word).collect();
        for label in &self.labels {
            if normalized.len() > label.len() && normalized[..label.len()] == label[..] {
                personal.extend(label.len()..words.len());
            }
        }
        personal.sort_unstable();
        personal.dedup();
        personal
    }

    /// Whether `words` are one of the labels and nothing else.
    fn bare_label(&self, words: &[String]) -> bool {
        let normalized: Vec<String> = words.iter().map(normalize_word).collect();
        self.labels.contains(&normalized)
    }
}

/// The block holding a bare label's value: the nearest block that starts right of the label
/// and whose vertical centre lies within the label's line.
fn value_beside(blocks: &[Block<RawRegion>], label: usize) -> Option<usize> {
    let line = blocks[label].region.shape.bbox()?;
    blocks
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != label)
        .filter_map(|(index, block)| Some((index, block.region.shape.bbox()?)))
        .filter(|(_, bbox)| {
            let centre = (bbox.y0 + bbox.y1) / 2.0;
            centre >= line.y0 && centre <= line.y1 && bbox.x0 >= line.x1 - line.height() / 2.0
        })
        .min_by(|(_, a), (_, b)| a.x0.total_cmp(&b.x0))
        .map(|(index, _)| index)
}

/// A word lowercased, without the punctuation that ends a label.
fn normalize_word(word: impl AsRef<str>) -> String {
    word.as_ref().trim_end_matches([':', '.', ';']).to_lowercase()
}

#[async_trait]
impl Redactor for PersonalTextRedactor {
    fn class(&self) -> RedactionClass {
        RedactionClass::personal_text()
    }

    async fn find(&self, cx: &VisionCx, image: &SentImage) -> Result<Vec<BBox>> {
        let reader = self.reader.get().await?;
        let descriptor = reader.descriptor();
        let fitted = fit_on_device(descriptor, image)?;
        let page = reader.read(cx, &fitted, &ReadOptions::default()).await?;
        let mut raw: Vec<RawRegion> = Vec::new();
        for (index, block) in page.blocks.iter().enumerate() {
            let texts: Vec<String> = if block.words.is_empty() {
                block.text.split_whitespace().map(str::to_string).collect()
            } else {
                block.words.iter().map(|w| w.text.clone()).collect()
            };
            if self.bare_label(&texts) {
                if let Some(value) = value_beside(&page.blocks, index) {
                    raw.push(page.blocks[value].region.clone());
                }
                continue;
            }
            let personal = self.personal_words(&texts);
            if block.words.is_empty() {
                if !personal.is_empty() {
                    raw.push(block.region.clone());
                }
                continue;
            }
            raw.extend(personal.into_iter().map(|i| block.words[i].region.clone()));
        }
        to_image_boxes(descriptor, &fitted, image, raw)
    }
}
