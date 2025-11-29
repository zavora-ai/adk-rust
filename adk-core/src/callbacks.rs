use crate::{CallbackContext, Content, LlmRequest, LlmResponse, ReadonlyContext, Result};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

// Agent callbacks
pub type BeforeAgentCallback = Box<dyn Fn(Arc<dyn CallbackContext>) -> Pin<Box<dyn Future<Output = Result<Option<Content>>> + Send>> + Send + Sync>;
pub type AfterAgentCallback = Box<dyn Fn(Arc<dyn CallbackContext>) -> Pin<Box<dyn Future<Output = Result<Option<Content>>> + Send>> + Send + Sync>;

/// Result from a BeforeModel callback
#[derive(Debug)]
pub enum BeforeModelResult {
    /// Continue with the (possibly modified) request
    Continue(LlmRequest),
    /// Skip the model call and use this response instead
    Skip(LlmResponse),
}

// Model callbacks
// BeforeModelCallback can modify the request or skip the model call entirely
pub type BeforeModelCallback = Box<dyn Fn(Arc<dyn CallbackContext>, LlmRequest) -> Pin<Box<dyn Future<Output = Result<BeforeModelResult>> + Send>> + Send + Sync>;
pub type AfterModelCallback = Box<dyn Fn(Arc<dyn CallbackContext>, LlmResponse) -> Pin<Box<dyn Future<Output = Result<Option<LlmResponse>>> + Send>> + Send + Sync>;

// Tool callbacks  
pub type BeforeToolCallback = Box<dyn Fn(Arc<dyn CallbackContext>) -> Pin<Box<dyn Future<Output = Result<Option<Content>>> + Send>> + Send + Sync>;
pub type AfterToolCallback = Box<dyn Fn(Arc<dyn CallbackContext>) -> Pin<Box<dyn Future<Output = Result<Option<Content>>> + Send>> + Send + Sync>;

// Instruction providers - dynamic instruction generation
pub type InstructionProvider = Box<dyn Fn(Arc<dyn ReadonlyContext>) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> + Send + Sync>;
pub type GlobalInstructionProvider = InstructionProvider;
