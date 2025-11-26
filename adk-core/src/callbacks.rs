use crate::{types::Content, CallbackContext, LlmRequest, LlmResponse, Result};
use futures::future::BoxFuture;
use serde_json::Value;
use std::sync::Arc;

// Agent callbacks
pub type BeforeAgentCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>) -> BoxFuture<'static, Result<Option<Content>>> + Send + Sync,
>;

pub type AfterAgentCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>) -> BoxFuture<'static, Result<Option<Content>>> + Send + Sync,
>;

// Model callbacks
pub type BeforeModelCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>, LlmRequest) -> BoxFuture<'static, Result<Option<LlmResponse>>>
        + Send
        + Sync,
>;

pub type AfterModelCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>, LlmResponse) -> BoxFuture<'static, Result<Option<LlmResponse>>>
        + Send
        + Sync,
>;

// Tool callbacks  
pub type BeforeToolCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>, String, Value) -> BoxFuture<'static, Result<Option<Value>>>
        + Send
        + Sync,
>;

pub type AfterToolCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>, String, Value) -> BoxFuture<'static, Result<Option<Value>>>
        + Send
        + Sync,
>;
