//! Webhook handlers for external event delivery.
//!
//! | Module | Feature | Purpose |
//! |--------|---------|---------|
//! | [`ingress`] | `webhook-ingress` | `POST /hooks/{name}`: a signed delivery enqueues a durable run |
//! | [`forge`] | `webhook-ingress` | GitHub's and GitLab's push signatures for a hook (spec 3.5 §7.8.2) |
//! | `openai` | `openai-webhooks` | OpenAI background-response events, broadcast in process |

#[cfg(feature = "openai-webhooks")]
pub mod openai;

#[cfg(feature = "openai-webhooks")]
pub use openai::{OpenAIWebhookConfig, OpenAIWebhookHandler, WebhookEvent};

#[cfg(feature = "webhook-ingress")]
pub mod forge;
#[cfg(feature = "webhook-ingress")]
pub mod ingress;

#[cfg(feature = "webhook-ingress")]
pub use forge::{ForgeSecret, ForgeSignature};

#[cfg(feature = "webhook-ingress")]
pub use ingress::{
    DEFAULT_HOOK_BODY_LIMIT, HookAccepted, HookDelivery, WebhookHook, WebhookIngress,
    webhook_ingress_router,
};
