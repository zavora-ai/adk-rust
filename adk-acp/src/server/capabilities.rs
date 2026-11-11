//! Builds the official ACP v1 capability declaration for the ADK-Rust agent.

pub use agent_client_protocol::v1::schema::AgentCapabilities;
use agent_client_protocol::v1::schema::{
    AgentAuthCapabilities, AuthStatusCapabilities, LogoutCapabilities, PromptCapabilities,
    SessionAdditionalDirectoriesCapabilities, SessionArchiveCapabilities, SessionCapabilities,
    SessionCloseCapabilities, SessionDeleteCapabilities, SessionForkCapabilities,
    SessionListCapabilities, SessionOptionsCapabilities, SessionResumeCapabilities,
    SessionStatusCapabilities, SessionSystemPromptCapabilities,
    SessionSystemPromptModeCapabilities, SessionSystemPromptOverrideCapabilities,
    SessionTitleCapabilities,
};

use super::config::{AcpServerConfig, SystemPromptSupport};

/// Constructs capabilities that exactly match the registered server handlers.
pub struct CapabilitiesBuilder;

impl CapabilitiesBuilder {
    /// Build the ACP v1 capability set implemented by the server.
    ///
    /// Every advertised capability has a registered handler (Capability_Accuracy):
    ///
    /// - `loadSession`, and the session `list`, `delete`, `resume`, `close`,
    ///   `fork`, `additionalDirectories`, `archive`, `status`, `options` and
    ///   `title` capabilities, each backed by its `session/*` handler;
    /// - prompt `image`, `audio` and `embeddedContext`, the content the prompt
    ///   handler accepts (see [`crate::content::block_to_part`]);
    /// - `systemPrompt`, with `override` unless the configuration narrows it to
    ///   `append` or disables it ([`SystemPromptSupport`]);
    /// - `auth.status` always, because `auth/status` answers whether or not an
    ///   authenticator is configured, and `auth.logout` only when the
    ///   configured [`AcpAuthenticator`](super::AcpAuthenticator) supports it.
    pub fn build(config: &AcpServerConfig) -> AgentCapabilities {
        let mut session = SessionCapabilities::new()
            .list(SessionListCapabilities::new())
            .delete(SessionDeleteCapabilities::new())
            .additional_directories(SessionAdditionalDirectoriesCapabilities::new())
            .resume(SessionResumeCapabilities::new())
            .fork(SessionForkCapabilities::new())
            .close(SessionCloseCapabilities::new())
            .archive(SessionArchiveCapabilities::new())
            .status(SessionStatusCapabilities::new())
            .options(SessionOptionsCapabilities::new())
            .title(SessionTitleCapabilities::new());
        session = match config.system_prompts {
            SystemPromptSupport::Disabled => session,
            SystemPromptSupport::Append => {
                session.system_prompt(SessionSystemPromptCapabilities::new())
            }
            SystemPromptSupport::AppendAndOverride => session.system_prompt(
                SessionSystemPromptCapabilities::new().mode(
                    SessionSystemPromptModeCapabilities::new()
                        .override_(SessionSystemPromptOverrideCapabilities::new()),
                ),
            ),
        };

        let mut auth = AgentAuthCapabilities::new().status(AuthStatusCapabilities::new());
        if config
            .authenticator
            .as_ref()
            .is_some_and(|authenticator| authenticator.supports_logout())
        {
            auth = auth.logout(LogoutCapabilities::new());
        }

        AgentCapabilities::new()
            .load_session(true)
            .prompt_capabilities(
                PromptCapabilities::new().embedded_context(true).image(true).audio(true),
            )
            .session_capabilities(session)
            .auth(auth)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use agent_client_protocol::v1::schema::{
        SessionConfigOption, SessionId, SessionMode, SessionModeState,
    };
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::server::config::AcpServerConfigBuilder;
    use crate::server::handler::AcpSessionHandler;
    use crate::server::modes::SessionControls;
    use crate::server::test_helpers::mock_agent_and_session;

    /// Session controls advertising both a mode set and a config option, used to
    /// exercise the provider-gated half of capability accuracy.
    struct ModesAndConfig;

    impl SessionControls for ModesAndConfig {
        fn modes(&self) -> Option<SessionModeState> {
            Some(SessionModeState::new(
                "ask",
                vec![SessionMode::new("ask", "Ask"), SessionMode::new("code", "Code")],
            ))
        }

        fn config_options(&self) -> Vec<SessionConfigOption> {
            vec![SessionConfigOption::boolean("verbose", "Verbose", false)]
        }
    }

    /// **Feature: acp-v1-full-support, Property 11: Capability accuracy**
    /// *For any* build configuration, an advertised capability implies a
    /// registered handler / enabled content mapping, and vice versa. The
    /// [`AgentCapabilities`] produced by [`CapabilitiesBuilder::build`] must
    /// correspond exactly to the handlers registered in
    /// [`crate::server::transport::stdio::serve_connection`] and the content
    /// mappings enabled by [`crate::content::block_to_part`]:
    ///
    /// - `embedded_context` / `image` / `audio` prompt capabilities ⇔ the prompt
    ///   handler accepts embedded-resource / image / audio content;
    /// - `load_session` ⇔ a `session/load` handler is registered;
    /// - session `fork` ⇔ a `session/fork` handler is registered;
    /// - session `list` / `delete` / `resume` / `close` / `additional_directories`
    ///   ⇔ their handlers are registered.
    ///
    /// **Validates: Requirements 13.1, 13.3**
    #[test]
    fn advertised_capabilities_correspond_to_registered_handlers() {
        let (agent, session_service) = mock_agent_and_session();
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_service)
            .build()
            .expect("valid config");
        let caps = CapabilitiesBuilder::build(&config);

        // Prompt content mappings enabled in `content::block_to_part`.
        assert!(
            caps.prompt_capabilities.embedded_context,
            "embedded_context must be advertised: the prompt handler accepts embedded resources"
        );
        assert!(
            caps.prompt_capabilities.image,
            "image must be advertised: the prompt handler accepts image content"
        );
        assert!(
            caps.prompt_capabilities.audio,
            "audio must be advertised: the prompt handler accepts audio content"
        );

        // `session/load` handler is registered in stdio.rs.
        assert!(caps.load_session, "load_session must be advertised: a load handler is registered");

        // Session handlers registered in stdio.rs.
        let session = &caps.session_capabilities;
        assert!(session.list.is_some(), "list handler is registered");
        assert!(session.delete.is_some(), "delete handler is registered");
        assert!(session.resume.is_some(), "resume handler is registered");
        assert!(session.close.is_some(), "close handler is registered");
        assert!(session.fork.is_some(), "fork handler is registered");
        assert!(
            session.additional_directories.is_some(),
            "additional-directories support is implemented in the session handlers"
        );
    }

    /// **Feature: acp-v1-full-support, Property 11: Capability accuracy (final audit)**
    /// The advertised prompt-content capabilities correspond to the shared
    /// content mapping *in both directions* — never more, never less
    /// (Requirement 13.1). For each content type the shared
    /// [`crate::content::block_to_part`] mapping recognizes, the corresponding
    /// prompt capability is advertised *if and only if* the mapping accepts that
    /// content type:
    ///
    /// - `image` ⇔ an image block maps to a `Part`;
    /// - `audio` ⇔ an audio block maps to a `Part`;
    /// - `embedded_context` ⇔ an embedded-resource block maps to a `Part`.
    ///
    /// Text always maps and requires no capability flag. This is the
    /// vice-versa direction of P11 at the content-mapping level: an enabled
    /// mapping implies an advertised capability, complementing the
    /// advertised ⇒ implemented direction exercised by
    /// [`crate::server::handler`]'s `advertised_prompt_capabilities_match_accepted_content_types`.
    ///
    /// **Validates: Requirements 13.1, 13.3**
    #[test]
    fn advertised_prompt_capabilities_match_content_mappings_bidirectionally() {
        use agent_client_protocol::v1::schema::{
            AudioContent, ContentBlock, EmbeddedResource as AcpEmbeddedResource,
            EmbeddedResourceResource, ImageContent, TextContent,
            TextResourceContents as AcpTextResourceContents,
        };
        use base64::{Engine as _, engine::general_purpose};

        use crate::content::block_to_part;

        let (agent, session_service) = mock_agent_and_session();
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_service)
            .build()
            .expect("valid config");
        let caps = CapabilitiesBuilder::build(&config);
        let prompt = &caps.prompt_capabilities;

        // image capability <=> the mapping accepts an image block.
        let image = ContentBlock::Image(ImageContent::new(
            general_purpose::STANDARD.encode([0x89, 0x50, 0x4E, 0x47]),
            "image/png",
        ));
        assert_eq!(
            block_to_part(&image).is_ok(),
            prompt.image,
            "image capability must be advertised iff the mapping accepts image content"
        );

        // audio capability <=> the mapping accepts an audio block.
        let audio = ContentBlock::Audio(AudioContent::new(
            general_purpose::STANDARD.encode([1u8, 2, 3, 4]),
            "audio/mp3",
        ));
        assert_eq!(
            block_to_part(&audio).is_ok(),
            prompt.audio,
            "audio capability must be advertised iff the mapping accepts audio content"
        );

        // embedded_context capability <=> the mapping accepts an embedded resource.
        let embedded = ContentBlock::Resource(AcpEmbeddedResource::new(
            EmbeddedResourceResource::TextResourceContents(AcpTextResourceContents::new(
                "fn main() {}",
                "file:///main.rs",
            )),
        ));
        assert_eq!(
            block_to_part(&embedded).is_ok(),
            prompt.embedded_context,
            "embedded_context must be advertised iff the mapping accepts embedded resources"
        );

        // Text is always representable and needs no capability flag.
        assert!(
            block_to_part(&ContentBlock::Text(TextContent::new("hi"))).is_ok(),
            "text content must always map"
        );
    }

    /// **Feature: acp-v1-full-support, Property 11: Capability accuracy**
    /// Modes and configuration options are advertised *if and only if* a
    /// [`SessionControls`] provider is configured (Requirement 13.1). A server
    /// built without a provider advertises no modes and no config options; a
    /// server built with a provider surfaces both in the session snapshot that
    /// drives `session/new` / `load` / `resume` / `fork` responses.
    ///
    /// **Validates: Requirements 13.1, 13.3**
    #[tokio::test]
    async fn modes_and_config_advertised_iff_session_controls_present() {
        let probe_id = SessionId::new("capability-probe");

        // Without a provider: no modes, no config options.
        let (agent, session_service) = mock_agent_and_session();
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_service)
            .build()
            .expect("valid config");
        let handler =
            Arc::new(AcpSessionHandler::new(&config, CancellationToken::new()).expect("handler"));
        let (modes, config_options) = handler.session_controls_snapshot(&probe_id).await;
        assert!(modes.is_none(), "no session_controls => no modes advertised");
        assert!(config_options.is_none(), "no session_controls => no config options advertised");

        // With a provider: both are advertised, reflecting the declared defaults.
        let (agent, session_service) = mock_agent_and_session();
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_service)
            .session_controls(Arc::new(ModesAndConfig))
            .build()
            .expect("valid config");
        let handler =
            Arc::new(AcpSessionHandler::new(&config, CancellationToken::new()).expect("handler"));
        let (modes, config_options) = handler.session_controls_snapshot(&probe_id).await;
        let modes = modes.expect("modes advertised when a provider is present");
        assert_eq!(modes.current_mode_id.to_string(), "ask");
        let config_options = config_options.expect("config options advertised when present");
        assert!(
            config_options.iter().any(|option| option.id.to_string() == "verbose"),
            "the declared verbose option must be advertised"
        );
    }
}
