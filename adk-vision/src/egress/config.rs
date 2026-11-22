//! `.adk/vision.toml`: egress after redaction.

use serde::{Deserialize, Serialize};

use super::{DataClass, RedactionClass};
use crate::error::{Result, VisionError};

/// What happens to an image bound for a destination, always after redaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EgressAction {
    /// Release it.
    Allow,
    /// Hold it until a person approves the exact image.
    Ask,
    /// Never release it.
    Deny,
}

/// One `[[egress.rule]]`: the action for a host and, optionally, some data classes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EgressRuleConfig {
    /// A host name, or `*.example.com` for its subdomains.
    pub host: String,
    /// The data classes the rule covers; empty covers every class.
    #[serde(default)]
    pub classes: Vec<DataClass>,
    /// The action.
    pub action: EgressAction,
}

impl EgressRuleConfig {
    fn matches(&self, host: &str, class: &DataClass) -> bool {
        let host_matches = match self.host.strip_prefix("*.") {
            Some(suffix) => {
                host.len() > suffix.len()
                    && host.ends_with(suffix)
                    && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
            }
            None => self.host.eq_ignore_ascii_case(host),
        };
        host_matches && (self.classes.is_empty() || self.classes.contains(class))
    }
}

/// The `[egress]` table: the default action, the redaction classes every release passes
/// through, and the rules per host and data class.
///
/// There is no setting that releases before redaction, and unknown keys are refused rather
/// than ignored (V-I2).
///
/// # Example
///
/// ```
/// use adk_vision::{DataClass, EgressAction, VisionConfig};
///
/// let config = VisionConfig::from_toml_str(r#"
///     [egress]
///     default = "ask"
///     redact = ["faces", "personal_text"]
///
///     [[egress.rule]]
///     host = "generativelanguage.googleapis.com"
///     classes = ["document_crop"]
///     action = "allow"
/// "#).unwrap();
/// let (action, rule) = config
///     .egress
///     .decide("generativelanguage.googleapis.com", &DataClass::new("document_crop"));
/// assert_eq!((action, rule), (EgressAction::Allow, Some(0)));
/// assert_eq!(config.egress.decide("api.anthropic.com", &DataClass::new("photo")).0, EgressAction::Ask);
/// assert!(VisionConfig::from_toml_str("[egress]\nallow_before_redaction = true").is_err());
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EgressConfig {
    /// The action when no rule matches. `ask` by default (spec 3.7 Q1).
    #[serde(default = "ask")]
    pub default: EgressAction,
    /// The redaction classes every release passes through.
    #[serde(default)]
    pub redact: Vec<RedactionClass>,
    /// The rules, first match wins.
    #[serde(default, rename = "rule")]
    pub rules: Vec<EgressRuleConfig>,
    /// The on-device profile: every remote destination is refused at the gate's first step,
    /// whatever the rules say, so no image leaves the machine. `false` by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub on_device: bool,
}

fn ask() -> EgressAction {
    EgressAction::Ask
}

impl Default for EgressConfig {
    fn default() -> Self {
        Self { default: EgressAction::Ask, redact: Vec::new(), rules: Vec::new(), on_device: false }
    }
}

impl EgressConfig {
    /// The on-device profile: every release is refused at the destination step with its
    /// reason, before anything is minimised, redacted or sent.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_vision::{EgressConfig, VisionConfig};
    ///
    /// let config = VisionConfig::from_toml_str("[egress]\non_device = true").unwrap();
    /// assert_eq!(config.egress, EgressConfig::default().on_device());
    /// ```
    pub fn on_device(mut self) -> Self {
        self.on_device = true;
        self
    }

    /// Sets the default action.
    pub fn with_default(mut self, action: EgressAction) -> Self {
        self.default = action;
        self
    }

    /// Requires a redaction class on every release.
    pub fn redacting(mut self, class: RedactionClass) -> Self {
        if !self.redact.contains(&class) {
            self.redact.push(class);
        }
        self
    }

    /// Appends a rule.
    pub fn with_rule(mut self, host: &str, classes: &[&str], action: EgressAction) -> Self {
        self.rules.push(EgressRuleConfig {
            host: host.to_string(),
            classes: classes.iter().map(|&c| DataClass::new(c)).collect(),
            action,
        });
        self
    }

    /// The action for an image of `class` bound for `host`, and the index of the rule that
    /// decided it (`None` for the default).
    pub fn decide(&self, host: &str, class: &DataClass) -> (EgressAction, Option<usize>) {
        self.rules
            .iter()
            .position(|rule| rule.matches(host, class))
            .map_or((self.default, None), |index| (self.rules[index].action, Some(index)))
    }
}

/// The contents of `.adk/vision.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisionConfig {
    /// The `[egress]` table.
    #[serde(default)]
    pub egress: EgressConfig,
    /// The `[sam]` table: which decoder turns `sam-3.1` masks into rasters.
    #[serde(default)]
    pub sam: crate::mask_decoder::SamSettings,
}

impl VisionConfig {
    /// Parses the TOML text of `.adk/vision.toml`.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::InvalidInput`] for invalid TOML or an unknown key.
    pub fn from_toml_str(text: &str) -> Result<Self> {
        toml::from_str(text)
            .map_err(|e| VisionError::InvalidInput(format!(".adk/vision.toml is invalid: {e}")))
    }
}
