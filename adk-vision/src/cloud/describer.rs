//! [`LlmDescriber`]: any `adk_core::Llm` that takes images, as a [`Describer`].

use std::marker::PhantomData;
use std::sync::Arc;

use adk_core::{
    Content, DataLocality, Effort, FinishReason, GenerateContentConfig, Llm, LlmRequest, Part,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

use super::adapter::{RegionAdapter, adapter_for_model, extract_json};
use super::preflight::{Preflight, Preflighted};
use crate::backend::{BackendDescriptor, InputLimits, Locality, PriceRef};
use crate::cx::VisionCx;
use crate::egress::ReleasedImage;
use crate::error::{Result, VisionError};
use crate::geometry::CoordFrame;
use crate::sent::{SentImage, VisionInput};
use crate::traits::{DescribeRequest, Describer, Description, ImageRegion};

/// A [`Describer`] over any `adk_core::Llm` that takes images: Gemini, Claude, Muse, an
/// NVIDIA NIM, or a model on this machine.
///
/// It sends the images as inline parts, the caller's schema as the response schema, and,
/// when regions are asked for, the region format of a [`RegionAdapter`] picked by model ID
/// ([`adapter_for_model`]). The descriptor's frame and input limits are the adapter's, so
/// the runtime maps every region to source pixels and the egress gate never sends an image
/// the provider would resize. A model with no adapter answers in text and values only; a
/// request for regions is refused with `Unsupported` rather than a guessed frame.
///
/// The model call is metered as one model call under the call's [`VisionCx`] scope: its
/// spend hold is sized by [`preflight`](Self::preflight) from the provider's image-token
/// formula, the prompt and the output cap, and its tokens are committed from the
/// response. The runtime records the call's images as units.
///
/// A remote describer ([`remote`](Self::remote)) takes only [`ReleasedImage`]s; a local
/// one ([`local`](Self::local)), for a model in this process or on a loopback address,
/// takes [`SentImage`]s.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_model::MockLlm;
/// use adk_vision::{CoordFrame, Describer, LlmDescriber};
///
/// let model = Arc::new(MockLlm::new("claude-sonnet-5-5"));
/// let describer = LlmDescriber::remote_with_hosts(model, ["api.anthropic.com"]).unwrap();
/// assert_eq!(describer.descriptor().frame, CoordFrame::SentPixels);
/// assert_eq!(describer.descriptor().limits.max_long_edge, Some(2576));
/// ```
pub struct LlmDescriber<I = ReleasedImage> {
    llm: Arc<dyn Llm>,
    descriptor: BackendDescriptor,
    adapter: Option<Arc<dyn RegionAdapter>>,
    effort: Option<Effort>,
    max_output_tokens: Option<u32>,
    unpriced_hold: Option<u64>,
    input: PhantomData<fn() -> I>,
}

impl<I> std::fmt::Debug for LlmDescriber<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmDescriber")
            .field("model", &self.llm.name())
            .field("descriptor", &self.descriptor)
            .field("adapter", &self.adapter.as_ref().map(|a| a.name()))
            .finish_non_exhaustive()
    }
}

impl LlmDescriber<ReleasedImage> {
    /// A describer for a model off the machine; it takes only images the egress gate
    /// released to it.
    ///
    /// The hosts and providers come from the model ([`Llm::data_destinations`]): every
    /// `adk-model` provider names the host of its configured endpoint, so a custom base URL,
    /// a Vertex location or a Bedrock region is the host the egress policy and rules see.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] when the model names no host; pass them with
    /// [`remote_with_hosts`](Self::remote_with_hosts).
    ///
    /// # Example
    ///
    /// ```
    /// use std::sync::Arc;
    /// use adk_model::anthropic::AnthropicConfig;
    /// use adk_model::AnthropicClient;
    /// use adk_vision::{Locality, LlmDescriber};
    ///
    /// let claude = AnthropicClient::new(AnthropicConfig::new("key", "claude-sonnet-5-5")).unwrap();
    /// let describer = LlmDescriber::remote(Arc::new(claude)).unwrap();
    /// let descriptor = adk_vision::Describer::descriptor(&describer);
    /// assert_eq!(descriptor.locality, Locality::Remote { hosts: vec!["api.anthropic.com".into()] });
    /// assert_eq!(descriptor.providers, ["anthropic"]);
    /// ```
    pub fn remote(llm: Arc<dyn Llm>) -> Result<Self> {
        Self::remote_with_hosts(llm, Vec::<String>::new())
    }

    /// A describer for a model off the machine that sends to `hosts` in place of the hosts
    /// the model names, for a model that names none or a gateway its client does not know
    /// about. With no `hosts`, the model's.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] when neither `hosts` nor the model names a host: the
    /// egress policy checks each host before an image is released.
    pub fn remote_with_hosts<H: Into<String>>(
        llm: Arc<dyn Llm>,
        hosts: impl IntoIterator<Item = H>,
    ) -> Result<Self> {
        let destinations = llm.data_destinations();
        let mut hosts: Vec<String> = hosts.into_iter().map(Into::into).collect();
        if hosts.is_empty() {
            hosts = adk_core::DataDestination::all_hosts(&destinations);
        }
        if hosts.is_empty() {
            return Err(VisionError::InvalidInput(format!(
                "model `{}` names no host it sends to, so the egress policy cannot check \
                 one; pass them with `LlmDescriber::remote_with_hosts`",
                llm.name()
            )));
        }
        let mut describer = Self::with_locality(llm, Locality::Remote { hosts });
        for destination in destinations {
            describer.descriptor = describer.descriptor.with_provider(destination.provider);
        }
        Ok(describer)
    }
}

impl LlmDescriber<SentImage> {
    /// A describer for a model in this process or on a loopback address, such as
    /// `adk-mistralrs` or a `mistralrs serve` endpoint on `127.0.0.1`.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] when the model reports that it sends data off the
    /// machine ([`Llm::data_locality`]); use [`LlmDescriber::remote`] so its images pass the
    /// egress gate.
    pub fn local(llm: Arc<dyn Llm>) -> Result<Self> {
        let locality = match llm.data_locality() {
            DataLocality::InProcess => Locality::InProcess,
            DataLocality::Loopback => Locality::Loopback,
            _ => {
                return Err(VisionError::InvalidInput(format!(
                    "model `{}` sends data off the machine; describe with \
                     `LlmDescriber::remote` so its images pass the egress gate",
                    llm.name()
                )));
            }
        };
        Ok(Self::with_locality(llm, locality))
    }
}

impl<I> LlmDescriber<I> {
    fn with_locality(llm: Arc<dyn Llm>, locality: Locality) -> Self {
        let adapter = adapter_for_model(llm.name());
        let mut describer = Self {
            descriptor: BackendDescriptor::new(llm.name(), locality, CoordFrame::SentPixels),
            llm,
            adapter: None,
            effort: None,
            max_output_tokens: None,
            unpriced_hold: None,
            input: PhantomData,
        };
        describer.set_adapter(adapter);
        describer
    }

    fn set_adapter(&mut self, adapter: Option<Arc<dyn RegionAdapter>>) {
        let model = self.llm.name().to_string();
        let vendor = adapter
            .as_ref()
            .and_then(|a| a.vendor().map(str::to_string))
            .unwrap_or_else(|| adk_core::vendor_for_model(&model).to_string());
        self.descriptor.frame = adapter.as_ref().map_or(CoordFrame::SentPixels, |a| a.frame());
        self.descriptor.limits =
            adapter.as_ref().map_or_else(InputLimits::unlimited, |a| a.limits());
        self.descriptor.price = match self.descriptor.locality {
            Locality::Remote { .. } => Some(PriceRef::Tokens { vendor, model }),
            _ => None,
        };
        self.adapter = adapter;
        self.refresh_estimate();
    }

    /// The runtime holds this estimate before an image is released (V-I6); the model call
    /// then holds and commits its own tokens.
    fn refresh_estimate(&mut self) {
        self.descriptor.token_estimate = match (&self.descriptor.price, &self.adapter) {
            (Some(PriceRef::Tokens { .. }), Some(adapter)) => super::preflight::token_estimate(
                self.llm.name(),
                adapter.image_tokens(),
                self.max_output_tokens,
            ),
            _ => None,
        };
    }

    /// Reads regions with `adapter` instead of the one picked by model ID; the descriptor
    /// takes its frame and limits.
    pub fn with_adapter(mut self, adapter: Arc<dyn RegionAdapter>) -> Self {
        self.set_adapter(Some(adapter));
        self
    }

    /// Answers in text and values only, refusing requests for regions.
    pub fn without_regions(mut self) -> Self {
        self.set_adapter(None);
        self
    }

    /// Names the backend `id` instead of the model name.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.descriptor.id = crate::backend::BackendId::new(id);
        self
    }

    /// Names the manifest the model was loaded from.
    #[cfg(feature = "mistralrs")]
    pub(crate) fn with_manifest(mut self, id: &str) -> Self {
        self.descriptor.manifest = Some(id.to_string());
        self
    }

    /// Sets the input limits, for a provider whose limit is lower than its adapter's.
    pub fn with_limits(mut self, limits: InputLimits) -> Self {
        self.descriptor.limits = limits;
        self
    }

    /// Sets the reasoning effort of every call.
    pub fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = Some(effort);
        self
    }

    /// Caps output tokens when the request does not.
    pub fn with_max_output_tokens(mut self, tokens: u32) -> Self {
        self.max_output_tokens = Some(tokens);
        self.refresh_estimate();
        self
    }

    /// Holds and commits `micro_usd` per call when the `adk-model` catalog does not price
    /// the model, instead of the run's unpriced-call estimate. NVIDIA publishes no price
    /// for its hosted trial endpoints, so `NimConfig` describers hold 0 and
    /// are bounded by unit caps.
    pub fn with_unpriced_hold(mut self, micro_usd: u64) -> Self {
        self.unpriced_hold = Some(micro_usd);
        self
    }

    /// The region adapter, when the describer returns regions.
    pub fn adapter(&self) -> Option<&dyn RegionAdapter> {
        self.adapter.as_deref()
    }

    /// The model.
    pub fn model(&self) -> &Arc<dyn Llm> {
        &self.llm
    }

    /// The cost estimate a call on images of `sizes` (as sent) holds before its request:
    /// the provider's image tokens, the prompt bounded at one token per byte, and the
    /// output cap, priced from the `adk-model` catalog.
    ///
    /// # Example
    ///
    /// ```
    /// use std::sync::Arc;
    /// use adk_model::MockLlm;
    /// use adk_vision::{DescribeSpec, LlmDescriber};
    ///
    /// let model = Arc::new(MockLlm::new("claude-haiku-5-5"));
    /// let describer = LlmDescriber::remote_with_hosts(model, ["api.anthropic.com"]).unwrap();
    /// let spec = DescribeSpec::new("Where is the save button?").with_regions()
    ///     .with_max_output_tokens(1_000);
    /// let estimate = describer.preflight(&[(2356, 1530)], &spec);
    /// assert_eq!(estimate.image_tokens, 4675);
    /// assert!(estimate.max_cost_micro_usd.is_some());
    /// ```
    pub fn preflight(&self, sizes: &[(u32, u32)], spec: &crate::DescribeSpec) -> Preflight {
        let request = self.build_request(
            sizes.iter().map(|_| Part::Text { text: String::new() }).collect(),
            sizes,
            &spec.prompt,
            spec.output_schema.as_ref(),
            spec.want_regions,
            spec.max_output_tokens,
        );
        Preflight::estimate(self.llm.name(), &request, self.adapter.as_deref(), sizes)
    }

    fn build_request(
        &self,
        images: Vec<Part>,
        sizes: &[(u32, u32)],
        prompt: &str,
        schema: Option<&Value>,
        want_regions: bool,
        max_output_tokens: Option<u32>,
    ) -> LlmRequest {
        let mut text = prompt.to_string();
        let mut config = GenerateContentConfig {
            max_output_tokens: max_output_tokens
                .or(self.max_output_tokens)
                .and_then(|t| i32::try_from(t).ok()),
            effort: self.effort,
            ..GenerateContentConfig::default()
        };
        match (want_regions, &self.adapter) {
            (true, Some(adapter)) => {
                let mut item = adapter.region_schema();
                if sizes.len() > 1
                    && let Some(props) = item.get_mut("properties").and_then(Value::as_object_mut)
                {
                    props.insert("image".into(), json!({"type": "integer"}));
                }
                let mut properties = json!({
                    "regions": {"type": "array", "items": item}
                });
                let mut required = vec!["regions"];
                match schema {
                    Some(value) => {
                        properties["value"] = value.clone();
                        required.insert(0, "value");
                    }
                    None => {
                        properties["text"] = json!({"type": "string"});
                        required.insert(0, "text");
                    }
                }
                let wrapper = json!({
                    "type": "object",
                    "properties": properties,
                    "required": required,
                    "additionalProperties": false
                });
                text.push_str("\n\n");
                text.push_str(&adapter.instructions(sizes));
                text.push_str(&format!(
                    " Answer with JSON only, in this shape: {}.",
                    if schema.is_some() {
                        "{\"value\": <the answer>, \"regions\": [...]}"
                    } else {
                        "{\"text\": <the answer>, \"regions\": [...]}"
                    }
                ));
                if adapter.structured_output() {
                    config.response_schema = Some(wrapper);
                }
                adapter.configure(&mut config);
            }
            (_, adapter) => {
                if let Some(value) = schema {
                    config.response_schema = Some(value.clone());
                    text.push_str("\n\nAnswer with JSON only.");
                }
                if let Some(adapter) = adapter {
                    adapter.configure(&mut config);
                }
            }
        }
        let mut parts = images;
        parts.push(Part::Text { text });
        LlmRequest {
            config: Some(config),
            ..LlmRequest::new(self.llm.name(), vec![Content { role: "user".into(), parts }])
        }
    }

    async fn run<T: VisionInput>(
        &self,
        cx: &VisionCx,
        request: &DescribeRequest<T>,
        part: impl Fn(&T) -> Result<Part>,
    ) -> Result<Description<ImageRegion>> {
        cx.check()?;
        let adapter = match (request.want_regions, &self.adapter) {
            (true, None) => {
                return Err(VisionError::Unsupported(format!(
                    "`{}` has no region adapter, so its regions would be in an unknown frame; \
                     ask it for text and values, or give the describer a `RegionAdapter`",
                    self.llm.name()
                )));
            }
            (true, Some(adapter)) => Some(adapter.as_ref()),
            (false, _) => None,
        };
        if request.images.is_empty() {
            return Err(VisionError::InvalidInput("describe needs at least one image".into()));
        }
        let sizes: Vec<(u32, u32)> = request.images.iter().map(|i| i.sent().size()).collect();
        let images = request.images.iter().map(&part).collect::<Result<Vec<_>>>()?;
        let llm_request = self.build_request(
            images,
            &sizes,
            &request.prompt,
            request.output_schema.as_ref(),
            request.want_regions,
            request.max_output_tokens,
        );
        let preflight =
            Preflight::estimate(self.llm.name(), &llm_request, self.adapter.as_deref(), &sizes);
        let bound = preflight.max_cost_micro_usd.or(self.unpriced_hold);
        let model = Preflighted::new(Arc::clone(&self.llm), bound);
        let mut stream = cx.scope().generate(&model, llm_request, false).await?;
        let mut text = String::new();
        let mut partial = String::new();
        let mut finish = None;
        while let Some(response) = stream.next().await {
            let response = response?;
            if response.finish_reason.is_some() {
                finish = response.finish_reason;
            }
            let target = if response.partial { &mut partial } else { &mut text };
            for p in response.content.iter().flat_map(|c| c.parts.iter()) {
                if let Part::Text { text } = p {
                    target.push_str(text);
                }
            }
        }
        if text.is_empty() {
            text = partial;
        }
        let backend = self.descriptor.id.to_string();
        match finish {
            Some(FinishReason::Refusal | FinishReason::Safety) => {
                return Err(VisionError::Backend {
                    backend,
                    message: format!("the model declined to answer ({finish:?}): {text}"),
                });
            }
            Some(FinishReason::MaxTokens) if text.trim().is_empty() => {
                return Err(VisionError::Backend {
                    backend,
                    message: "the answer was empty at the output cap; raise \
                              `max_output_tokens` or lower the model's reasoning"
                        .into(),
                });
            }
            _ => {}
        }
        parse_answer(&backend, &text, adapter, request.output_schema.is_some(), &sizes)
    }
}

/// Reads a model's text into a description.
fn parse_answer(
    backend: &str,
    text: &str,
    adapter: Option<&dyn RegionAdapter>,
    has_schema: bool,
    sizes: &[(u32, u32)],
) -> Result<Description<ImageRegion>> {
    let parse = |message: String| VisionError::Parse { backend: backend.to_string(), message };
    let Some(adapter) = adapter else {
        if has_schema {
            let value = extract_json(text)
                .ok_or_else(|| parse(format!("the answer holds no JSON value: {text}")))?;
            return Ok(Description { value: Some(value), ..Description::default() });
        }
        return Ok(Description { text: Some(text.to_string()), ..Description::default() });
    };
    let root =
        extract_json(text).ok_or_else(|| parse(format!("the answer holds no JSON: {text}")))?;
    let (items, answer) = match &root {
        Value::Array(items) => (items.clone(), None),
        Value::Object(fields) => {
            let items = ["regions", "objects", "boxes"]
                .iter()
                .find_map(|k| fields.get(*k)?.as_array().cloned())
                .ok_or_else(|| parse(format!("the answer has no `regions` list: {root}")))?;
            let answer = if has_schema { fields.get("value") } else { fields.get("text") };
            (items, answer.cloned())
        }
        other => return Err(parse(format!("the answer is not an object: {other}"))),
    };
    let mut regions = Vec::with_capacity(items.len());
    for item in &items {
        if let Some(region) = adapter.parse_region(item, sizes).map_err(&parse)? {
            regions.push(region);
        }
    }
    let mut description = Description { regions, ..Description::default() };
    match (has_schema, answer) {
        (true, value) => description.value = value,
        (false, Some(Value::String(s))) => description.text = Some(s),
        (false, Some(other)) => description.text = Some(other.to_string()),
        (false, None) => {}
    }
    Ok(description)
}

fn released_part(image: &ReleasedImage) -> Result<Part> {
    check_inline(image.bytes().len(), image.receipt().backend.as_str())?;
    Ok(image.to_part())
}

fn sent_part(image: &SentImage) -> Result<Part> {
    let bytes = image.encode_png()?;
    check_inline(bytes.len(), "local describer")?;
    Ok(Part::InlineData {
        mime_type: "image/png".into(),
        data: bytes,
        uri: None,
        annotations: None,
    })
}

fn check_inline(len: usize, backend: &str) -> Result<()> {
    if len > adk_core::MAX_INLINE_DATA_SIZE {
        return Err(VisionError::InputTooLarge {
            backend: backend.to_string(),
            limit: format!("{} bytes inline", adk_core::MAX_INLINE_DATA_SIZE),
            actual: format!("{len} bytes"),
        });
    }
    Ok(())
}

#[async_trait]
impl Describer<ReleasedImage> for LlmDescriber<ReleasedImage> {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn describe(
        &self,
        cx: &VisionCx,
        request: &DescribeRequest<ReleasedImage>,
    ) -> Result<Description<ImageRegion>> {
        if let Some(other) = request.images.iter().find(|i| !i.is_for(&self.descriptor.id)) {
            return Err(VisionError::NotReleased {
                backend: format!(
                    "{} (the image was released to `{}`)",
                    self.descriptor.id,
                    other.receipt().backend
                ),
            });
        }
        self.run(cx, request, released_part).await
    }
}

#[async_trait]
impl Describer<SentImage> for LlmDescriber<SentImage> {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn describe(
        &self,
        cx: &VisionCx,
        request: &DescribeRequest<SentImage>,
    ) -> Result<Description<ImageRegion>> {
        self.run(cx, request, sent_part).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::{ClaudePixels, GeminiBox2d};
    use crate::geometry::{BBox, Shape};

    #[test]
    fn answers_are_read_with_the_adapter() {
        let claude = ClaudePixels::for_model("claude-sonnet-5-5");
        let text = r#"{"text": "two boxes", "regions": [
            {"label": "red", "box": [100, 80, 260, 200]},
            {"label": "blue", "box": [380, 260, 560, 420]}]}"#;
        let d = parse_answer("c", text, Some(&claude), false, &[(640, 480)]).unwrap();
        assert_eq!(d.text.as_deref(), Some("two boxes"));
        assert_eq!(d.regions.len(), 2);
        assert_eq!(d.regions[1].region.shape, Shape::Box(BBox::new(380.0, 260.0, 560.0, 420.0)));
    }

    #[test]
    fn values_and_regions_come_back_together() {
        let gemini = GeminiBox2d::for_model("gemini-3.8-flash");
        let text = r#"```json
{"value": {"kind": "invoice"}, "regions": [{"label": "total", "box_2d": [1, 2, 3, 4]}]}
```"#;
        let d = parse_answer("g", text, Some(&gemini), true, &[(10, 10)]).unwrap();
        assert_eq!(d.value, Some(json!({"kind": "invoice"})));
        assert_eq!(d.regions.len(), 1);
    }

    #[test]
    fn a_malformed_answer_is_refused() {
        let claude = ClaudePixels::for_model("claude-sonnet-5-5");
        for text in [
            "I cannot see any boxes.",
            r#"{"text": "x"}"#,
            r#"{"text": "x", "regions": [{"label": "a", "box": [1, 2]}]}"#,
        ] {
            let err = parse_answer("c", text, Some(&claude), false, &[(10, 10)]).unwrap_err();
            assert!(matches!(err, VisionError::Parse { .. }), "{text}: {err}");
        }
    }

    #[test]
    fn text_without_an_adapter_is_the_answer() {
        let d = parse_answer("m", "A receipt.", None, false, &[(10, 10)]).unwrap();
        assert_eq!(d.text.as_deref(), Some("A receipt."));
    }
}
