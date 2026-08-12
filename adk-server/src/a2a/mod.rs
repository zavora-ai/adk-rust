pub mod agent_card;
pub mod client;
pub mod events;
pub mod executor;
pub mod jsonrpc;
pub mod metadata;
pub mod parts;
pub mod processor;
pub mod remote_agent;
pub mod types;

pub use agent_card::{agent_skills_from_index, build_agent_card, build_agent_skills};
pub use client::A2aClient;
pub use events::{event_to_message, message_to_event};
pub use executor::{Executor, ExecutorConfig};
pub use jsonrpc::{
    JsonRpcError, JsonRpcRequest, JsonRpcResponse, MessageSendConfig, MessageSendParams, Task,
    TasksCancelParams, TasksGetParams,
};
pub use metadata::{InvocationMeta, to_invocation_meta};
pub use parts::{a2a_parts_to_adk, adk_parts_to_a2a};
pub use remote_agent::{RemoteA2aAgent, RemoteA2aAgentBuilder, RemoteA2aConfig};
pub use types::*;

#[cfg(feature = "a2a-interceptors")]
pub mod audit_log;
#[cfg(feature = "a2a-interceptors")]
pub mod bearer_auth;
#[cfg(feature = "a2a-interceptors")]
pub mod interceptor;
#[cfg(feature = "a2a-interceptors")]
pub mod rate_limit;
#[cfg(feature = "a2a-interceptors")]
pub use audit_log::AuditLogInterceptor;
#[cfg(feature = "a2a-interceptors")]
pub use bearer_auth::{BearerAuthInterceptor, TokenValidator};
#[cfg(feature = "a2a-interceptors")]
pub use interceptor::{
    A2aDelegationContext, A2aError as A2aInterceptorError, A2aInterceptor, InterceptorChain,
    InterceptorDecision,
};
#[cfg(feature = "a2a-interceptors")]
pub use rate_limit::RateLimitInterceptor;

#[cfg(feature = "a2a-v1")]
pub mod convenience;
#[cfg(feature = "a2a-v1")]
pub use convenience::{A2aServer, A2aServerApp, A2aServerBuilder};
#[cfg(feature = "a2a-v1")]
pub mod v1;
