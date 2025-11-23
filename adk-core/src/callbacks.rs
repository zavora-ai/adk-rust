use crate::{types::Content, CallbackContext, Result};
use futures::future::BoxFuture;
use std::sync::Arc;

pub type BeforeAgentCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>) -> BoxFuture<'static, Result<Option<Content>>> + Send + Sync,
>;

pub type AfterAgentCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>) -> BoxFuture<'static, Result<Option<Content>>> + Send + Sync,
>;
