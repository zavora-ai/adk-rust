//! Typed models for OpenRouter discovery endpoints.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

type JsonMap = BTreeMap<String, serde_json::Value>;

/// OpenRouter numeric fields that may arrive as JSON strings or numbers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum OpenRouterBigNumber {
    String(String),
    Integer(i64),
    Float(f64),
}

/// Envelope returned by `GET /models`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterModelsEnvelope {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data: Vec<OpenRouterModel>,
}

/// One OpenRouter model descriptor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hugging_face_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<OpenRouterModelArchitecture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<OpenRouterModelPricing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_provider: Option<OpenRouterTopProviderInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_request_limits: Option<OpenRouterPerRequestLimits>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_parameters: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_parameters: Option<OpenRouterDefaultParameters>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiration_date: Option<String>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Architecture metadata for a discovered model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterModelArchitecture {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokenizer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruct_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modality: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_modalities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_modalities: Vec<String>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Price metadata returned by discovery endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterModelPricing {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<OpenRouterBigNumber>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<OpenRouterBigNumber>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<OpenRouterBigNumber>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<OpenRouterBigNumber>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_token: Option<OpenRouterBigNumber>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_output: Option<OpenRouterBigNumber>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<OpenRouterBigNumber>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_output: Option<OpenRouterBigNumber>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_audio_cache: Option<OpenRouterBigNumber>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<OpenRouterBigNumber>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub internal_reasoning: Option<OpenRouterBigNumber>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cache_read: Option<OpenRouterBigNumber>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cache_write: Option<OpenRouterBigNumber>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discount: Option<f64>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Envelope returned by `GET /models/{author}/{slug}/endpoints`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterModelEndpointsEnvelope {
    pub data: OpenRouterModelEndpoints,
}

/// Endpoint-discovery payload returned for one model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterModelEndpoints {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<OpenRouterModelArchitecture>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<OpenRouterModelEndpoint>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// One provider endpoint for a discovered model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterModelEndpoint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_prompt_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_parameters: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<OpenRouterEndpointStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_last_30m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_implicit_caching: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_last_30m: Option<OpenRouterPercentileStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput_last_30m: Option<OpenRouterPercentileStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<OpenRouterModelPricing>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Top-provider metadata attached to a discovered model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterTopProviderInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_moderated: Option<bool>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Per-request token limits returned from model discovery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterPerRequestLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<i64>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Default parameter values attached to a discovered model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterDefaultParameters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Endpoint status codes returned by OpenRouter discovery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum OpenRouterEndpointStatus {
    Code(i32),
    Label(String),
}

/// Percentile metrics for endpoint latency and throughput.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterPercentileStats {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p50: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p75: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p90: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p99: Option<f64>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Envelope returned by `GET /providers`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterProvidersEnvelope {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data: Vec<OpenRouterProvider>,
}

/// One provider entry from `GET /providers`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterProvider {
    pub name: String,
    pub slug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy_policy_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terms_of_service_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_page_url: Option<String>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Envelope returned by `GET /credits`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterCreditsEnvelope {
    pub data: OpenRouterCredits,
}

/// Credit balance and usage totals.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterCredits {
    pub total_credits: f64,
    pub total_usage: f64,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

#[cfg(test)]
mod tests {
    use super::{
        OpenRouterBigNumber, OpenRouterEndpointStatus, OpenRouterModelEndpointsEnvelope,
        OpenRouterModelsEnvelope,
    };

    #[test]
    fn models_envelope_parses_architecture_pricing_and_supported_parameters() {
        let envelope: OpenRouterModelsEnvelope = serde_json::from_value(serde_json::json!({
            "data": [
                {
                    "id": "openai/gpt-4.1",
                    "canonical_slug": "openai/gpt-4.1",
                    "hugging_face_id": null,
                    "name": "GPT-4.1",
                    "created": 1_710_000_000,
                    "description": "General-purpose model",
                    "pricing": {
                        "prompt": "0.000002",
                        "completion": 0.000008,
                        "image_token": "0.000001",
                        "web_search": "0.01",
                        "discount": 0.25
                    },
                    "context_length": 128000,
                    "architecture": {
                        "tokenizer": "GPT",
                        "instruct_type": "chatml",
                        "modality": "text->text",
                        "input_modalities": ["text", "image"],
                        "output_modalities": ["text"]
                    },
                    "top_provider": {
                        "context_length": 128000,
                        "max_completion_tokens": 16384,
                        "is_moderated": true
                    },
                    "per_request_limits": {
                        "prompt_tokens": 8000,
                        "completion_tokens": 4000
                    },
                    "supported_parameters": ["temperature", "web_search_options"],
                    "default_parameters": {
                        "temperature": 0.7,
                        "top_p": 0.9,
                        "frequency_penalty": 0.1
                    },
                    "expiration_date": "2026-12-01"
                }
            ]
        }))
        .expect("models envelope should deserialize");

        let model = envelope.data.first().expect("model should exist");
        assert_eq!(model.supported_parameters, vec!["temperature", "web_search_options"]);
        assert_eq!(
            model.architecture.as_ref().map(|architecture| architecture.input_modalities.clone()),
            Some(vec!["text".to_string(), "image".to_string()])
        );
        assert_eq!(
            model.pricing.as_ref().and_then(|pricing| pricing.image_token.as_ref()),
            Some(&OpenRouterBigNumber::String("0.000001".to_string()))
        );
        assert_eq!(
            model.default_parameters.as_ref().and_then(|defaults| defaults.temperature),
            Some(0.7)
        );
    }

    #[test]
    fn model_endpoints_envelope_parses_latency_throughput_and_status_variants() {
        let numeric_status: OpenRouterModelEndpointsEnvelope =
            serde_json::from_value(serde_json::json!({
                "data": {
                    "id": "openai/gpt-4.1",
                    "name": "GPT-4.1",
                    "created": 1_710_000_000,
                    "description": "General-purpose model",
                    "architecture": {
                        "tokenizer": "GPT",
                        "instruct_type": "chatml",
                        "modality": "text->text",
                        "input_modalities": ["text"],
                        "output_modalities": ["text"]
                    },
                    "endpoints": [
                        {
                            "name": "OpenAI: GPT-4.1",
                            "model_id": "openai/gpt-4.1",
                            "model_name": "GPT-4.1",
                            "context_length": 128000,
                            "pricing": {
                                "prompt": "0.000002",
                                "completion": "0.000008"
                            },
                            "provider_name": "OpenAI",
                            "tag": "openai",
                            "quantization": "fp16",
                            "max_completion_tokens": 16384,
                            "max_prompt_tokens": 128000,
                            "supported_parameters": ["temperature"],
                            "status": 0,
                            "uptime_last_30m": 99.9,
                            "supports_implicit_caching": true,
                            "latency_last_30m": { "p50": 120.0, "p99": 900.0 },
                            "throughput_last_30m": { "p50": 80.0, "p99": 10.0 }
                        }
                    ]
                }
            }))
            .expect("endpoints envelope should deserialize");

        let endpoint = numeric_status.data.endpoints.first().expect("endpoint should exist");
        assert_eq!(endpoint.supports_implicit_caching, Some(true));
        assert_eq!(endpoint.latency_last_30m.as_ref().and_then(|latency| latency.p99), Some(900.0));
        assert_eq!(
            endpoint.throughput_last_30m.as_ref().and_then(|throughput| throughput.p50),
            Some(80.0)
        );
        assert_eq!(endpoint.status, Some(OpenRouterEndpointStatus::Code(0)));

        let string_status: OpenRouterModelEndpointsEnvelope =
            serde_json::from_value(serde_json::json!({
                "data": {
                    "id": "openai/gpt-4.1",
                    "name": "GPT-4.1",
                    "created": 1_710_000_000,
                    "description": "General-purpose model",
                    "architecture": {
                        "modality": "text->text",
                        "input_modalities": ["text"],
                        "output_modalities": ["text"]
                    },
                    "endpoints": [
                        {
                            "name": "OpenAI: GPT-4.1",
                            "model_id": "openai/gpt-4.1",
                            "model_name": "GPT-4.1",
                            "context_length": 128000,
                            "pricing": {
                                "prompt": "0.000002",
                                "completion": "0.000008"
                            },
                            "provider_name": "OpenAI",
                            "tag": "openai",
                            "supported_parameters": [],
                            "status": "default",
                            "supports_implicit_caching": false
                        }
                    ]
                }
            }))
            .expect("string status should deserialize");

        assert_eq!(
            string_status.data.endpoints[0].status,
            Some(OpenRouterEndpointStatus::Label("default".to_string()))
        );
    }
}
