//! Usage read from the provider's own JSON object.
//!
//! A count that is `null`, missing, or not an integer is unknown, never a reason to fail the
//! turn, and the object is kept as received in [`UsageMetadata::provider_usage`].

#[cfg(any(feature = "openai", feature = "groq", feature = "deepseek"))]
use adk_core::UsageMetadata;
use serde_json::Value;

/// The count at JSON `pointer` in `usage`; `None` when it is missing, `null`, or no `i32`.
pub(crate) fn count(usage: &Value, pointer: &str) -> Option<i32> {
    i32::try_from(usage.pointer(pointer)?.as_i64()?).ok()
}

/// Normalizes a Chat Completions `usage` object, the shape OpenAI and the compatible
/// providers (Groq, DeepSeek, and the presets) report.
#[cfg(any(feature = "openai", feature = "groq", feature = "deepseek"))]
pub(crate) fn chat_completions(usage: &Value) -> Option<UsageMetadata> {
    if !usage.is_object() {
        return None;
    }
    let prompt = count(usage, "/prompt_tokens").unwrap_or_default();
    let completion = count(usage, "/completion_tokens").unwrap_or_default();
    Some(UsageMetadata {
        prompt_token_count: prompt,
        candidates_token_count: completion,
        total_token_count: count(usage, "/total_tokens")
            .unwrap_or_else(|| prompt.saturating_add(completion)),
        cache_read_input_token_count: count(usage, "/prompt_tokens_details/cached_tokens"),
        cache_creation_input_token_count: count(usage, "/prompt_tokens_details/cache_write_tokens"),
        thinking_token_count: count(usage, "/completion_tokens_details/reasoning_tokens"),
        audio_input_token_count: count(usage, "/prompt_tokens_details/audio_tokens"),
        audio_output_token_count: count(usage, "/completion_tokens_details/audio_tokens"),
        provider_usage: Some(usage.clone()),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn count_reads_integers_and_treats_everything_else_as_unknown() {
        let usage = json!({"a": 7, "b": null, "c": "7", "d": {"e": 2}, "f": 1.5});
        assert_eq!(count(&usage, "/a"), Some(7));
        assert_eq!(count(&usage, "/b"), None);
        assert_eq!(count(&usage, "/c"), None);
        assert_eq!(count(&usage, "/d/e"), Some(2));
        assert_eq!(count(&usage, "/f"), None);
        assert_eq!(count(&usage, "/missing"), None);
        assert_eq!(count(&json!({"big": u64::MAX}), "/big"), None);
    }

    #[cfg(any(feature = "openai", feature = "groq", feature = "deepseek"))]
    #[test]
    fn null_chat_counts_read_as_zero_and_the_total_falls_back_to_the_sum() {
        let usage = json!({
            "prompt_tokens": 12,
            "completion_tokens": 4,
            "total_tokens": null,
            "prompt_tokens_details": {"cached_tokens": null},
            "completion_tokens_details": null,
            "queue_time": 0.25
        });
        assert_eq!(
            chat_completions(&usage).map(|meta| serde_json::to_value(meta).unwrap()),
            Some(json!({
                "prompt_token_count": 12,
                "candidates_token_count": 4,
                "total_token_count": 16,
                "provider_usage": usage
            }))
        );
        assert!(chat_completions(&Value::Null).is_none());
    }
}
