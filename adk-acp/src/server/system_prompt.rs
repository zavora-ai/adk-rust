//! Client-provided system prompts (`systemPrompt` on session lifecycle requests).
//!
//! ACP lets a client send system-level instructions with `session/new`,
//! `session/load`, `session/resume` and `session/fork` when the agent
//! advertises `sessionCapabilities.systemPrompt`. The server resolves the
//! prompt when the session becomes active and applies it to every model
//! request of the session's turns through an [`InvocationHooks`] on the run's
//! [`RunConfig`](adk_core::RunConfig):
//!
//! - `append` adds the prompt after the agent's own system instructions;
//! - `override` replaces them.
//!
//! The prompt lasts while the session is active on the connection. A later
//! load or resume without `systemPrompt` uses the agent's own instructions
//! again, as the protocol specifies.

use std::sync::Arc;

use adk_core::{BeforeModelResult, CallbackContext, Content, InvocationHooks, LlmRequest};
use agent_client_protocol::v1::schema::{
    ContentBlock, EmbeddedResourceResource, SystemPrompt, SystemPromptMode,
};
use async_trait::async_trait;

use super::config::SystemPromptSupport;
use super::error::AcpServerError;

/// How a resolved system prompt combines with the agent's own instructions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PromptMode {
    Append,
    Override,
}

/// A system prompt the server accepted for one active session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SystemPromptHook {
    mode: PromptMode,
    text: String,
}

impl SystemPromptHook {
    /// Resolve the `systemPrompt` of a session request against what the server
    /// supports.
    ///
    /// Returns `Ok(None)` when the client sent none. The protocol requires an
    /// agent that cannot apply a prompt — an unadvertised mode or content type —
    /// to fail the request rather than set the session up without it.
    pub(crate) fn resolve(
        prompt: Option<&SystemPrompt>,
        support: SystemPromptSupport,
    ) -> Result<Option<Arc<Self>>, AcpServerError> {
        let Some(prompt) = prompt else {
            return Ok(None);
        };
        let mode = match (&prompt.mode, support) {
            (_, SystemPromptSupport::Disabled) => {
                return Err(AcpServerError::Unsupported(
                    "this agent does not accept client system prompts".into(),
                ));
            }
            (SystemPromptMode::Append, _) => PromptMode::Append,
            (SystemPromptMode::Override, SystemPromptSupport::AppendAndOverride) => {
                PromptMode::Override
            }
            (SystemPromptMode::Override, _) => {
                return Err(AcpServerError::Unsupported(
                    "this agent accepts system prompts in append mode only".into(),
                ));
            }
            (other, _) => {
                return Err(AcpServerError::Unsupported(format!(
                    "unsupported system prompt mode: {other:?}"
                )));
            }
        };

        let mut parts = Vec::with_capacity(prompt.content.len());
        for block in &prompt.content {
            match block {
                ContentBlock::Text(text) => parts.push(text.text.clone()),
                ContentBlock::Resource(resource) => match &resource.resource {
                    EmbeddedResourceResource::TextResourceContents(contents) => {
                        parts.push(format!(
                            "<resource uri=\"{}\">\n{}\n</resource>",
                            contents.uri, contents.text
                        ));
                    }
                    _ => {
                        return Err(AcpServerError::Unsupported(
                            "system prompts accept text and embedded text resources only".into(),
                        ));
                    }
                },
                _ => {
                    return Err(AcpServerError::Unsupported(
                        "system prompts accept text and embedded text resources only".into(),
                    ));
                }
            }
        }
        let text = parts.join("\n\n");
        if text.trim().is_empty() {
            return Err(AcpServerError::MalformedMessage("systemPrompt content is empty".into()));
        }
        Ok(Some(Arc::new(Self { mode, text })))
    }

    /// Apply the prompt to one model request.
    ///
    /// `LlmAgent` sends its own instructions as the leading `system` contents of
    /// the request. `append` inserts the prompt after them; `override` replaces
    /// them.
    pub(crate) fn apply(&self, request: &mut LlmRequest) {
        let leading_system =
            request.contents.iter().take_while(|content| content.role == "system").count();
        let prompt = Content::new("system").with_text(self.text.clone());
        match self.mode {
            PromptMode::Append => request.contents.insert(leading_system, prompt),
            PromptMode::Override => {
                request.contents.splice(..leading_system, [prompt]);
            }
        }
    }
}

#[async_trait]
impl InvocationHooks for SystemPromptHook {
    async fn before_model(
        &self,
        _ctx: Arc<dyn CallbackContext>,
        mut request: LlmRequest,
    ) -> adk_core::Result<BeforeModelResult> {
        self.apply(&mut request);
        Ok(BeforeModelResult::Continue(request))
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::v1::schema::{EmbeddedResource, ImageContent, TextResourceContents};

    use super::*;

    fn request() -> LlmRequest {
        LlmRequest::new(
            "model",
            vec![
                Content::new("system").with_text("agent instruction"),
                Content::new("system").with_text("transfer guidance"),
                Content::new("user").with_text("hello"),
            ],
        )
    }

    fn texts(request: &LlmRequest) -> Vec<(String, String)> {
        request
            .contents
            .iter()
            .map(|content| {
                let text = content
                    .parts
                    .iter()
                    .filter_map(|part| part.text())
                    .collect::<Vec<_>>()
                    .join("");
                (content.role.clone(), text)
            })
            .collect()
    }

    #[test]
    fn no_prompt_resolves_to_none() {
        assert!(SystemPromptHook::resolve(None, SystemPromptSupport::Disabled).unwrap().is_none());
    }

    #[test]
    fn append_goes_after_the_agents_own_instructions() {
        let prompt = SystemPrompt::new(vec![ContentBlock::from("be terse")]);
        let hook =
            SystemPromptHook::resolve(Some(&prompt), SystemPromptSupport::Append).unwrap().unwrap();
        let mut request = request();
        hook.apply(&mut request);
        assert_eq!(
            texts(&request),
            vec![
                ("system".into(), "agent instruction".into()),
                ("system".into(), "transfer guidance".into()),
                ("system".into(), "be terse".into()),
                ("user".into(), "hello".into()),
            ]
        );
    }

    #[test]
    fn override_replaces_the_agents_own_instructions() {
        let prompt = SystemPrompt::new(vec![ContentBlock::from("only this")])
            .mode(SystemPromptMode::Override);
        let hook = SystemPromptHook::resolve(Some(&prompt), SystemPromptSupport::AppendAndOverride)
            .unwrap()
            .unwrap();
        let mut request = request();
        hook.apply(&mut request);
        assert_eq!(
            texts(&request),
            vec![("system".into(), "only this".into()), ("user".into(), "hello".into())]
        );
    }

    #[test]
    fn override_is_refused_when_only_append_is_supported() {
        let prompt =
            SystemPrompt::new(vec![ContentBlock::from("x")]).mode(SystemPromptMode::Override);
        let error =
            SystemPromptHook::resolve(Some(&prompt), SystemPromptSupport::Append).unwrap_err();
        assert!(matches!(error, AcpServerError::Unsupported(_)));
    }

    #[test]
    fn any_prompt_is_refused_when_disabled() {
        let prompt = SystemPrompt::new(vec![ContentBlock::from("x")]);
        assert!(matches!(
            SystemPromptHook::resolve(Some(&prompt), SystemPromptSupport::Disabled),
            Err(AcpServerError::Unsupported(_))
        ));
    }

    #[test]
    fn embedded_text_resources_become_text_and_images_are_refused() {
        let resource = ContentBlock::Resource(EmbeddedResource::new(
            EmbeddedResourceResource::TextResourceContents(TextResourceContents::new(
                "rules",
                "file:///AGENTS.md",
            )),
        ));
        let prompt = SystemPrompt::new(vec![ContentBlock::from("intro"), resource]);
        let hook =
            SystemPromptHook::resolve(Some(&prompt), SystemPromptSupport::Append).unwrap().unwrap();
        assert!(hook.text.starts_with("intro\n\n<resource uri=\"file:///AGENTS.md\">"));
        assert!(hook.text.contains("rules"));

        let image =
            SystemPrompt::new(vec![ContentBlock::Image(ImageContent::new("AA", "image/png"))]);
        assert!(matches!(
            SystemPromptHook::resolve(Some(&image), SystemPromptSupport::Append),
            Err(AcpServerError::Unsupported(_))
        ));
    }

    #[test]
    fn an_empty_prompt_is_malformed() {
        let prompt = SystemPrompt::new(vec![ContentBlock::from("  ")]);
        assert!(matches!(
            SystemPromptHook::resolve(Some(&prompt), SystemPromptSupport::Append),
            Err(AcpServerError::MalformedMessage(_))
        ));
    }
}
