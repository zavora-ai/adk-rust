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

/// What an `[[egress.rule]]` matches: a host, or every host of one provider.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RuleTarget {
    /// A host name, or `*.example.com` for its subdomains.
    Host(String),
    /// A provider id, as a backend names it in
    /// [`BackendDescriptor::providers`](crate::BackendDescriptor::providers): the id its
    /// model's responses carry in `LlmResponse::provider`, such as `anthropic`, `gemini`,
    /// `bedrock` or `nvidia`.
    Provider(String),
}

/// One `[[egress.rule]]`: the action for a host or a provider and, optionally, some data
/// classes.
///
/// A rule names exactly one of `host` and `provider`; a rule with both or neither is
/// refused.
///
/// # Example
///
/// ```
/// use adk_vision::{EgressAction, RuleTarget, VisionConfig};
///
/// let config = VisionConfig::from_toml_str(r#"
///     [[egress.rule]]
///     provider = "anthropic"
///     action = "ask"
/// "#).unwrap();
/// assert_eq!(config.egress.rules[0].target, RuleTarget::Provider("anthropic".into()));
/// assert_eq!(config.egress.rules[0].action, EgressAction::Ask);
///
/// let both = r#"
///     [[egress.rule]]
///     host = "api.anthropic.com"
///     provider = "anthropic"
///     action = "ask"
/// "#;
/// assert!(VisionConfig::from_toml_str(both).is_err());
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RuleToml", into = "RuleToml")]
pub struct EgressRuleConfig {
    /// The host or provider the rule matches.
    pub target: RuleTarget,
    /// The data classes the rule covers; empty covers every class.
    pub classes: Vec<DataClass>,
    /// The action.
    pub action: EgressAction,
}

/// The TOML form of a rule: exactly one of `host` and `provider`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleToml {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(default)]
    classes: Vec<DataClass>,
    action: EgressAction,
}

impl TryFrom<RuleToml> for EgressRuleConfig {
    type Error = String;

    fn try_from(rule: RuleToml) -> std::result::Result<Self, Self::Error> {
        let target = match (rule.host, rule.provider) {
            (Some(host), None) => RuleTarget::Host(host),
            (None, Some(provider)) => RuleTarget::Provider(provider),
            (Some(_), Some(_)) => {
                return Err("an egress rule names `host` or `provider`, not both".into());
            }
            (None, None) => {
                return Err("an egress rule names the `host` or the `provider` it covers".into());
            }
        };
        Ok(Self { target, classes: rule.classes, action: rule.action })
    }
}

impl From<EgressRuleConfig> for RuleToml {
    fn from(rule: EgressRuleConfig) -> Self {
        let (host, provider) = match rule.target {
            RuleTarget::Host(host) => (Some(host), None),
            RuleTarget::Provider(provider) => (None, Some(provider)),
        };
        Self { host, provider, classes: rule.classes, action: rule.action }
    }
}

impl EgressRuleConfig {
    fn matches(&self, host: &str, providers: &[String], class: &DataClass) -> bool {
        let target_matches = match &self.target {
            RuleTarget::Host(pattern) => match pattern.strip_prefix("*.") {
                Some(suffix) => {
                    host.len() > suffix.len()
                        && host.ends_with(suffix)
                        && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
                }
                None => pattern.eq_ignore_ascii_case(host),
            },
            RuleTarget::Provider(provider) => {
                providers.iter().any(|p| p.eq_ignore_ascii_case(provider))
            }
        };
        target_matches && (self.classes.is_empty() || self.classes.contains(class))
    }
}

/// The `[egress]` table: the default action, the redaction classes every release passes
/// through, and the rules per host or provider and data class.
///
/// With no table, every image a remote backend is called with leaves unredacted and
/// unasked. Each restriction is an opt-in: `default = "ask"` or `"deny"`, a rule that asks
/// or denies for a host or provider, a `redact` class, or `on_device = true`. A chosen
/// restriction fails closed: a listed class no redactor covers refuses the release
/// ([`VisionError::RedactionUnavailable`]), and redaction runs before the decision, so an
/// approval names the redacted image. Unknown keys are refused rather than ignored (V-I2).
///
/// # Example
///
/// ```
/// use adk_vision::{DataClass, EgressAction, VisionConfig};
///
/// let open = VisionConfig::default();
/// assert_eq!(open.egress.decide("api.anthropic.com", &[], &DataClass::new("photo")).0, EgressAction::Allow);
/// assert!(open.egress.redact.is_empty());
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
///
///     [[egress.rule]]
///     provider = "openrouter"
///     action = "deny"
/// "#).unwrap();
/// let (action, rule) = config
///     .egress
///     .decide("generativelanguage.googleapis.com", &[], &DataClass::new("document_crop"));
/// assert_eq!((action, rule), (EgressAction::Allow, Some(0)));
/// let photo = DataClass::new("photo");
/// assert_eq!(config.egress.decide("api.anthropic.com", &[], &photo).0, EgressAction::Ask);
/// let openrouter = ["openrouter".to_string()];
/// assert_eq!(config.egress.decide("openrouter.ai", &openrouter, &photo), (EgressAction::Deny, Some(1)));
/// assert!(VisionConfig::from_toml_str("[egress]\nallow_before_redaction = true").is_err());
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EgressConfig {
    /// The action when no rule matches. `allow` by default; `ask` or `deny` opt in to a
    /// restriction.
    #[serde(default = "allow")]
    pub default: EgressAction,
    /// The redaction classes every release passes through; none by default.
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

fn allow() -> EgressAction {
    EgressAction::Allow
}

impl Default for EgressConfig {
    fn default() -> Self {
        Self {
            default: EgressAction::Allow,
            redact: Vec::new(),
            rules: Vec::new(),
            on_device: false,
        }
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

    /// Appends a rule for `host`, or `*.example.com` for its subdomains.
    pub fn with_rule(mut self, host: &str, classes: &[&str], action: EgressAction) -> Self {
        self.rules.push(EgressRuleConfig {
            target: RuleTarget::Host(host.to_string()),
            classes: classes.iter().map(|&c| DataClass::new(c)).collect(),
            action,
        });
        self
    }

    /// Appends a rule for every host of `provider`, a provider id such as `anthropic`.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_vision::{DataClass, EgressAction, EgressConfig};
    ///
    /// let config = EgressConfig::default().with_provider_rule("gemini", &[], EgressAction::Ask);
    /// let gemini = ["gemini".to_string()];
    /// let photo = DataClass::new("photo");
    /// assert_eq!(config.decide("us-central1-aiplatform.googleapis.com", &gemini, &photo).0, EgressAction::Ask);
    /// assert_eq!(config.decide("api.anthropic.com", &[], &photo).0, EgressAction::Allow);
    /// ```
    pub fn with_provider_rule(
        mut self,
        provider: &str,
        classes: &[&str],
        action: EgressAction,
    ) -> Self {
        self.rules.push(EgressRuleConfig {
            target: RuleTarget::Provider(provider.to_string()),
            classes: classes.iter().map(|&c| DataClass::new(c)).collect(),
            action,
        });
        self
    }

    /// The action for an image of `class` bound for `host` of a backend that sends to
    /// `providers`, and the index of the rule that decided it (`None` for the default).
    pub fn decide(
        &self,
        host: &str,
        providers: &[String],
        class: &DataClass,
    ) -> (EgressAction, Option<usize>) {
        self.rules
            .iter()
            .position(|rule| rule.matches(host, providers, class))
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
    /// Returns [`VisionError::InvalidInput`] for invalid TOML, an unknown key, or a rule that
    /// names both or neither of `host` and `provider`.
    pub fn from_toml_str(text: &str) -> Result<Self> {
        toml::from_str(text)
            .map_err(|e| VisionError::InvalidInput(format!(".adk/vision.toml is invalid: {e}")))
    }
}
