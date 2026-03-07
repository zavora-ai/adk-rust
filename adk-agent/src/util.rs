use adk_core::{event, model};

pub fn map_finish_reason(reason: Option<model::FinishReason>) -> Option<event::FinishReason> {
    reason
}

pub fn map_usage_metadata(usage: Option<model::UsageMetadata>) -> Option<event::UsageMetadata> {
    usage
}
