//! Session mode and configuration-option state model.
//!
//! An ADK agent optionally exposes session *modes* (e.g. "ask", "code") and
//! session *configuration options* (dropdowns, toggles) to ACP clients through
//! the [`SessionControls`] provider trait. The server advertises whatever the
//! provider declares — nothing more — so [`Capability_Accuracy`] is preserved:
//! an agent that declares no controls advertises no modes and no config options.
//!
//! [`Capability_Accuracy`]: crate::server
//!
//! # Persistence
//!
//! The selected mode and configuration values are stored in the ADK session
//! state so they survive `session/load`, `session/resume`, and `session/fork`:
//!
//! - the selected mode id under [`MODE_STATE_KEY`] (`acp:mode`);
//! - each configuration value under `acp:config:<id>` (see [`config_state_key`]).
//!
//! The provider's declared set is the source of truth for *what* modes and
//! options exist and their defaults; the session state records the *current*
//! selection layered on top of those defaults.

use agent_client_protocol::v1::schema::{
    SessionConfigId, SessionConfigKind, SessionConfigOption, SessionConfigOptionValue,
    SessionConfigSelectOptions, SessionModeId, SessionModeState,
};

// Re-export the SDK session-mode / config-option types so callers can build
// them without reaching into the SDK's module path directly.
pub use agent_client_protocol::v1::schema::{
    AvailableCommand, AvailableCommandInput, SessionConfigBoolean, SessionConfigGroupId,
    SessionConfigOptionCategory, SessionConfigSelect, SessionConfigSelectGroup,
    SessionConfigSelectOption, SessionConfigValueId, SessionMode, UnstructuredCommandInput,
};

/// Session-state key under which the selected mode id is stored.
///
/// The value is the string form of the [`SessionModeId`]. Persisting it here
/// means the selection survives load / resume / fork.
pub const MODE_STATE_KEY: &str = "acp:mode";

/// Prefix for session-state keys under which configuration values are stored.
///
/// The full key for a given option id is `acp:config:<id>` (see
/// [`config_state_key`]). The stored value is the JSON encoding of a
/// [`SessionConfigOptionValue`].
pub const CONFIG_STATE_KEY_PREFIX: &str = "acp:config:";

/// Build the session-state key under which a configuration option's value is
/// stored.
///
/// # Example
///
/// ```rust,ignore
/// use agent_client_protocol::v1::schema::SessionConfigId;
/// let key = adk_acp::server::modes::config_state_key(&SessionConfigId::new("model"));
/// assert_eq!(key, "acp:config:model");
/// ```
pub fn config_state_key(id: &SessionConfigId) -> String {
    format!("{CONFIG_STATE_KEY_PREFIX}{id}")
}

/// Provider of session modes and configuration options for an ADK agent exposed
/// over ACP.
///
/// The default implementation advertises no modes and no configuration options,
/// so an agent that does not implement this trait is treated as having no
/// interactive session controls. Provide an implementation to advertise modes
/// and/or options and to let clients switch between them via `session/set_mode`
/// and `session/set_config_option`.
///
/// # Example
///
/// ```rust,ignore
/// use adk_acp::server::modes::SessionControls;
/// use agent_client_protocol::v1::schema::{SessionMode, SessionModeState};
///
/// struct AskOrCode;
///
/// impl SessionControls for AskOrCode {
///     fn modes(&self) -> Option<SessionModeState> {
///         Some(SessionModeState::new(
///             "ask",
///             vec![SessionMode::new("ask", "Ask"), SessionMode::new("code", "Code")],
///         ))
///     }
/// }
/// ```
pub trait SessionControls: Send + Sync {
    /// The set of modes this agent supports and the default current mode.
    ///
    /// Returns `None` (the default) when the agent supports no session modes,
    /// in which case the server advertises no modes and rejects every
    /// `session/set_mode` request as referencing an unknown mode.
    fn modes(&self) -> Option<SessionModeState> {
        None
    }

    /// The configuration options this agent supports and their default values.
    ///
    /// Returns an empty vector (the default) when the agent exposes no
    /// configuration options, in which case the server advertises none and
    /// rejects every `session/set_config_option` request as referencing an
    /// unknown option.
    fn config_options(&self) -> Vec<SessionConfigOption> {
        Vec::new()
    }

    /// The named commands (ACP slash-commands) this agent exposes.
    ///
    /// ADK agents have no native slash-command concept, so commands are
    /// declared here rather than discovered from the agent. Returns an empty
    /// vector (the default) when the agent exposes no commands, in which case
    /// the server emits no [`AvailableCommandsUpdate`] on session activation.
    ///
    /// When non-empty, the server emits a
    /// [`SessionUpdate::AvailableCommandsUpdate`] carrying these commands each
    /// time a session becomes active (create / resume / load / fork). The same
    /// emission path is reused by any future command-set change trigger.
    ///
    /// [`AvailableCommandsUpdate`]: agent_client_protocol::v1::schema::AvailableCommandsUpdate
    /// [`SessionUpdate::AvailableCommandsUpdate`]: agent_client_protocol::v1::schema::SessionUpdate::AvailableCommandsUpdate
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_acp::server::modes::{AvailableCommand, SessionControls};
    ///
    /// struct WithCommands;
    ///
    /// impl SessionControls for WithCommands {
    ///     fn available_commands(&self) -> Vec<AvailableCommand> {
    ///         vec![AvailableCommand::new("plan", "Draft an execution plan")]
    ///     }
    /// }
    /// ```
    fn available_commands(&self) -> Vec<AvailableCommand> {
        Vec::new()
    }
}

/// Returns `true` when `mode_id` is one of the modes advertised in `state`.
pub(crate) fn mode_is_advertised(state: &SessionModeState, mode_id: &SessionModeId) -> bool {
    state.available_modes.iter().any(|mode| &mode.id == mode_id)
}

/// Returns `true` when `value` is a valid value for `option`.
///
/// A boolean option accepts a boolean value; a select option accepts a
/// value-id that appears among its declared choices (ungrouped or grouped).
/// Any other combination is rejected so an unknown value never mutates state.
pub(crate) fn config_value_is_valid(
    option: &SessionConfigOption,
    value: &SessionConfigOptionValue,
) -> bool {
    match (&option.kind, value) {
        (SessionConfigKind::Boolean(_), SessionConfigOptionValue::Boolean { .. }) => true,
        (SessionConfigKind::Select(select), SessionConfigOptionValue::ValueId { value }) => {
            select_contains_value(&select.options, value)
        }
        _ => false,
    }
}

/// Returns `true` when `value` appears among the select option's choices.
fn select_contains_value(
    options: &SessionConfigSelectOptions,
    value: &SessionConfigValueId,
) -> bool {
    match options {
        SessionConfigSelectOptions::Ungrouped(items) => {
            items.iter().any(|option| &option.value == value)
        }
        SessionConfigSelectOptions::Grouped(groups) => {
            groups.iter().any(|group| group.options.iter().any(|option| &option.value == value))
        }
        // The SDK marks this enum non-exhaustive; an unknown option shape has
        // no declared values we can match, so no value is considered valid.
        _ => false,
    }
}

/// Return a copy of `option` with its current value replaced by `value`.
///
/// Used to reflect a persisted selection back onto the provider-declared
/// option when building session responses and `ConfigOptionUpdate`
/// notifications. A value whose shape does not match the option kind is
/// ignored, leaving the declared default in place.
pub(crate) fn option_with_current_value(
    mut option: SessionConfigOption,
    value: &SessionConfigOptionValue,
) -> SessionConfigOption {
    match (&mut option.kind, value) {
        (SessionConfigKind::Boolean(boolean), SessionConfigOptionValue::Boolean { value }) => {
            boolean.current_value = *value;
        }
        (SessionConfigKind::Select(select), SessionConfigOptionValue::ValueId { value }) => {
            select.current_value = value.clone();
        }
        _ => {}
    }
    option
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::v1::schema::{SessionConfigOption, SessionMode, SessionModeState};

    use super::*;

    struct NoControls;
    impl SessionControls for NoControls {}

    /// The default `SessionControls` implementation advertises nothing, so an
    /// agent that opts out has no modes and no config options.
    #[test]
    fn default_session_controls_advertise_nothing() {
        let controls = NoControls;
        assert!(controls.modes().is_none());
        assert!(controls.config_options().is_empty());
        assert!(controls.available_commands().is_empty());
    }

    #[test]
    fn config_state_key_prefixes_the_option_id() {
        assert_eq!(config_state_key(&SessionConfigId::new("model")), "acp:config:model");
        assert_eq!(config_state_key(&SessionConfigId::new("verbose")), "acp:config:verbose");
    }

    #[test]
    fn mode_is_advertised_matches_only_declared_modes() {
        let state = SessionModeState::new(
            "ask",
            vec![SessionMode::new("ask", "Ask"), SessionMode::new("code", "Code")],
        );
        assert!(mode_is_advertised(&state, &SessionModeId::new("ask")));
        assert!(mode_is_advertised(&state, &SessionModeId::new("code")));
        assert!(!mode_is_advertised(&state, &SessionModeId::new("autonomous")));
    }

    #[test]
    fn select_config_value_validation_accepts_only_declared_values() {
        let option = SessionConfigOption::select(
            "model",
            "Model",
            "fast",
            vec![
                SessionConfigSelectOption::new("fast", "Fast"),
                SessionConfigSelectOption::new("smart", "Smart"),
            ],
        );
        assert!(config_value_is_valid(&option, &SessionConfigOptionValue::value_id("fast")));
        assert!(config_value_is_valid(&option, &SessionConfigOptionValue::value_id("smart")));
        assert!(!config_value_is_valid(&option, &SessionConfigOptionValue::value_id("genius")));
        // A boolean value for a select option is the wrong shape.
        assert!(!config_value_is_valid(&option, &SessionConfigOptionValue::boolean(true)));
    }

    #[test]
    fn boolean_config_value_validation_requires_boolean_shape() {
        let option = SessionConfigOption::boolean("verbose", "Verbose", false);
        assert!(config_value_is_valid(&option, &SessionConfigOptionValue::boolean(true)));
        assert!(config_value_is_valid(&option, &SessionConfigOptionValue::boolean(false)));
        assert!(!config_value_is_valid(&option, &SessionConfigOptionValue::value_id("on")));
    }

    #[test]
    fn option_with_current_value_reflects_select_and_boolean_selections() {
        let select = SessionConfigOption::select(
            "model",
            "Model",
            "fast",
            vec![
                SessionConfigSelectOption::new("fast", "Fast"),
                SessionConfigSelectOption::new("smart", "Smart"),
            ],
        );
        let updated =
            option_with_current_value(select, &SessionConfigOptionValue::value_id("smart"));
        match updated.kind {
            SessionConfigKind::Select(select) => {
                assert_eq!(select.current_value, SessionConfigValueId::new("smart"));
            }
            other => panic!("expected a select option, got {other:?}"),
        }

        let boolean = SessionConfigOption::boolean("verbose", "Verbose", false);
        let updated = option_with_current_value(boolean, &SessionConfigOptionValue::boolean(true));
        match updated.kind {
            SessionConfigKind::Boolean(boolean) => assert!(boolean.current_value),
            other => panic!("expected a boolean option, got {other:?}"),
        }
    }
}
