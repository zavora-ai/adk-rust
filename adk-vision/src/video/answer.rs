//! What the analyzers that ask a model share: one metered model call, its usage, and the
//! answer format.

use std::sync::Arc;

use adk_core::{FinishReason, Llm, LlmRequest, Part, UsageMetadata};
use futures::StreamExt;
use serde_json::{Value, json};

use crate::backend::{BackendDescriptor, Locality, PriceRef};
use crate::cloud::Preflighted;
use crate::cx::VisionCx;
use crate::error::{Result, VisionError};
use crate::traits::VideoUsage;

/// A model's text and the usage it reported.
pub(crate) struct ModelAnswer {
    pub text: String,
    pub usage: Option<UsageMetadata>,
}

/// One model call under the call's scope, with its spend hold capped at `bound`.
pub(crate) async fn generate(
    cx: &VisionCx,
    llm: &Arc<dyn Llm>,
    bound: Option<u64>,
    request: LlmRequest,
    backend: &str,
) -> Result<ModelAnswer> {
    cx.check()?;
    let model = Preflighted::new(Arc::clone(llm), bound);
    let mut stream = cx.scope().generate(&model, request, false).await?;
    let (mut text, mut partial, mut finish, mut usage) = (String::new(), String::new(), None, None);
    while let Some(response) = stream.next().await {
        let response = response?;
        if response.finish_reason.is_some() {
            finish = response.finish_reason;
        }
        if response.usage_metadata.is_some() {
            usage = response.usage_metadata;
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
    match finish {
        Some(FinishReason::Refusal | FinishReason::Safety) => Err(VisionError::Backend {
            backend: backend.to_string(),
            message: format!("the model declined to answer ({finish:?}): {text}"),
        }),
        Some(FinishReason::MaxTokens) if text.trim().is_empty() => Err(VisionError::Backend {
            backend: backend.to_string(),
            message: "the answer was empty at the output cap; raise the analyzer's \
                      `max_output_tokens`"
                .into(),
        }),
        _ => Ok(ModelAnswer { text, usage }),
    }
}

/// Adds one model call's usage to `total`: tokens as reported, cost zero on the device,
/// the provider's reported cost, else the `adk-model` catalog's price, else unknown.
pub(crate) fn add_usage(
    total: &mut VideoUsage,
    descriptor: &BackendDescriptor,
    usage: Option<&UsageMetadata>,
) {
    total.requests += 1;
    let Some(usage) = usage else {
        if descriptor.locality.is_remote() {
            total.cost_micro_usd = None;
        }
        return;
    };
    let tokens = |n: i32| u64::try_from(n).unwrap_or(0);
    *total.input_tokens.get_or_insert(0) += tokens(usage.prompt_token_count);
    *total.output_tokens.get_or_insert(0) += tokens(usage.candidates_token_count);
    let cost = match (&descriptor.locality, &descriptor.price) {
        (Locality::InProcess | Locality::Loopback, _) => Some(0),
        (_, Some(PriceRef::Tokens { vendor, model })) => usage
            .cost
            .or_else(|| {
                adk_model::pricing::PricingCatalog::standard().cost_usd(
                    Some(vendor.as_str()),
                    model,
                    usage,
                )
            })
            .and_then(adk_core::usd_to_micro_usd),
        _ => usage.cost.and_then(adk_core::usd_to_micro_usd),
    };
    // One unpriced call makes the total unknown.
    total.cost_micro_usd = match (total.requests, total.cost_micro_usd, cost) {
        (1, _, cost) => cost,
        (_, Some(sum), Some(cost)) => Some(sum.saturating_add(cost)),
        _ => None,
    };
}

/// The JSON schema of an `Ask` answer: the answer in words and the events it names.
pub(crate) fn ask_schema(event_time: &str) -> Value {
    let time = match event_time {
        "image" => json!({"type": "integer", "description": "the number of the image, from 1"}),
        _ => json!({"type": "string", "description": "m:ss.s from the start of the video"}),
    };
    json!({
        "type": "object",
        "properties": {
            "answer": {"type": "string"},
            "events": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "start": time.clone(),
                        "end": time,
                        "text": {"type": "string"}
                    },
                    "required": ["start", "end", "text"]
                }
            }
        },
        "required": ["answer", "events"]
    })
}

/// Reads `m:ss`, `m:ss.s`, `h:mm:ss` or a number of seconds as milliseconds.
pub(crate) fn parse_time(value: &Value) -> Option<u64> {
    let seconds = match value {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => {
            let mut total = 0.0;
            for part in s.trim().trim_end_matches('s').split(':') {
                total = total * 60.0 + part.trim().parse::<f64>().ok()?;
            }
            total
        }
        _ => return None,
    };
    (seconds.is_finite() && seconds >= 0.0).then(|| (seconds * 1_000.0).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_in_every_form_a_model_writes() {
        assert_eq!(parse_time(&json!("1:23.4")), Some(83_400));
        assert_eq!(parse_time(&json!("01:02:03")), Some(3_723_000));
        assert_eq!(parse_time(&json!("12.5s")), Some(12_500));
        assert_eq!(parse_time(&json!(7)), Some(7_000));
        assert_eq!(parse_time(&json!("soon")), None);
        assert_eq!(parse_time(&json!(-1)), None);
    }
}
