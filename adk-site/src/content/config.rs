//! `site.toml`: the site's configuration (spec 3.5 §7.8.2).

use std::collections::BTreeMap;

use awp_types::{ContentKind, LanguageTag, Redirect};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Result, SiteError};
use crate::lanes::KindLanes;
use crate::translation::TranslationPolicy;

/// The `security.txt` fields (RFC 9116).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityContact {
    /// `Contact:` URIs, such as `mailto:security@example.com`. `security.txt` is generated
    /// only when at least one is set.
    pub contact: Vec<String>,
    /// `Expires:`; defaults to one year after the build time.
    pub expires: Option<DateTime<Utc>>,
    /// `Policy:` URL.
    pub policy: Option<String>,
    /// `Acknowledgments:` URL.
    pub acknowledgments: Option<String>,
}

/// The performance budget of every page (spec 3.5 §7.8.1 "Performance").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PerformanceBudget {
    /// Bytes of the HTML and its render-blocking resources; default 200 KB.
    pub critical_path_bytes: u64,
    /// Largest Contentful Paint on the reference profile, in milliseconds; recorded as not
    /// measured by the built-in gate, which renders nothing.
    pub lcp_ms: u32,
}

impl Default for PerformanceBudget {
    fn default() -> Self {
        Self { critical_path_bytes: 200_000, lcp_ms: 2_500 }
    }
}

/// A site's configuration, read from `site.toml` at the repository root.
///
/// # Example
///
/// ```rust
/// use adk_site::content::SiteConfig;
///
/// let config = SiteConfig::from_toml(r#"
/// name = "Example Solar"
/// domain = "example.com"
/// languages = ["en", "sw"]
/// jurisdictions = ["KE"]
/// "#).unwrap();
/// assert_eq!(config.default_language().as_str(), "en");
/// assert_eq!(config.base_url(), "https://example.com");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteConfig {
    /// The site's name.
    pub name: String,
    /// The domain the site is served at.
    pub domain: String,
    /// The origin URL; defaults to `https://<domain>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// A one-line description for `llms.txt` and the home page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The site's languages; the first is the default and is served without a prefix.
    pub languages: Vec<LanguageTag>,
    /// Jurisdictions whose privacy, consent and terms pages the launch gate requires.
    #[serde(default)]
    pub jurisdictions: Vec<String>,
    /// `security.txt`.
    #[serde(default)]
    pub security: SecurityContact,
    /// The performance budget.
    #[serde(default)]
    pub performance: PerformanceBudget,
    /// Redirects kept across releases, such as a migrated site's old URLs.
    #[serde(default)]
    pub redirects: Vec<Redirect>,
    /// Capability names that apply to every page's machine view.
    #[serde(default)]
    pub site_capabilities: Vec<String>,
    /// Capability names that apply to the pages of a content kind.
    #[serde(default)]
    pub kind_capabilities: BTreeMap<ContentKind, Vec<String>>,
    /// The read-only capability that serves each live field, such as
    /// `stock = "check_availability"` (AWP 2.0 §8.2).
    #[serde(default)]
    pub live_capabilities: BTreeMap<String, String>,
    /// Kinds whose latest items the home page lists.
    #[serde(default = "default_home_sections")]
    pub home_sections: Vec<ContentKind>,
    /// Items per listing page.
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    /// Interface strings per language, overriding the theme's.
    #[serde(default)]
    pub strings: BTreeMap<String, BTreeMap<String, String>>,
    /// How languages are translated and which must publish together.
    #[serde(default)]
    pub translation: TranslationPolicy,
    /// The charter version releases are built under.
    #[serde(default)]
    pub charter_version: u32,
    /// The lane of each field per content kind: live, stream, or released (spec 3.5 §7.8.3).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lanes: BTreeMap<ContentKind, KindLanes>,
}

fn default_home_sections() -> Vec<ContentKind> {
    vec![ContentKind::Post, ContentKind::Article, ContentKind::Notice, ContentKind::Event]
}

fn default_page_size() -> usize {
    50
}

impl SiteConfig {
    /// A configuration with defaults for `name`, `domain` and `languages`.
    ///
    /// # Panics
    ///
    /// Never; `languages` may be empty and fails [`validate`](Self::validate).
    pub fn new(
        name: impl Into<String>,
        domain: impl Into<String>,
        languages: Vec<LanguageTag>,
    ) -> Self {
        Self {
            name: name.into(),
            domain: domain.into(),
            base_url: None,
            description: None,
            languages,
            jurisdictions: Vec::new(),
            security: SecurityContact::default(),
            performance: PerformanceBudget::default(),
            redirects: Vec::new(),
            site_capabilities: Vec::new(),
            kind_capabilities: BTreeMap::new(),
            live_capabilities: BTreeMap::new(),
            home_sections: default_home_sections(),
            page_size: default_page_size(),
            strings: BTreeMap::new(),
            translation: TranslationPolicy::default(),
            charter_version: 0,
            lanes: BTreeMap::new(),
        }
    }

    /// Parses and validates `site.toml`.
    ///
    /// # Errors
    ///
    /// [`SiteError::InvalidConfig`] when the text is not valid TOML for the configuration
    /// or fails [`validate`](Self::validate).
    pub fn from_toml(text: &str) -> Result<Self> {
        let config: Self =
            toml::from_str(text).map_err(|e| SiteError::InvalidConfig(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Checks the configuration.
    ///
    /// # Errors
    ///
    /// [`SiteError::InvalidConfig`] for an empty name, domain or language list, a duplicate
    /// language, a domain with a scheme or path, a page size of zero, or a lane declaration
    /// that serves a claim outside the release lane (A-I26) or names a field in two lanes.
    pub fn validate(&self) -> Result<()> {
        let fail = |m: &str| Err(SiteError::InvalidConfig(m.to_string()));
        if self.name.trim().is_empty() {
            return fail("name is empty");
        }
        if self.domain.trim().is_empty() || self.domain.contains('/') || self.domain.contains(':') {
            return fail("domain must be a host name such as example.com, without scheme or path");
        }
        if self.languages.is_empty() {
            return fail("languages must list at least one language");
        }
        let mut seen = std::collections::BTreeSet::new();
        if !self.languages.iter().all(|l| seen.insert(l.as_str().to_ascii_lowercase())) {
            return fail("languages lists a language twice");
        }
        if self.page_size == 0 {
            return fail("page_size must be at least 1");
        }
        if let Some(violation) = crate::lanes::config_violations(self).into_iter().next() {
            return fail(&violation.to_string());
        }
        if let Some((kind, field)) = crate::lanes::overlapping(self).into_iter().next() {
            return fail(&format!(
                "[lanes.{}] declares '{field}' both volatile and stream; give it one lane",
                crate::content::kind_name(kind)
            ));
        }
        Ok(())
    }

    /// The default language.
    pub fn default_language(&self) -> &LanguageTag {
        &self.languages[0]
    }

    /// The origin URL without a trailing slash.
    pub fn base_url(&self) -> String {
        self.base_url
            .clone()
            .unwrap_or_else(|| format!("https://{}", self.domain))
            .trim_end_matches('/')
            .to_string()
    }

    /// The path prefix of `language`: empty for the default language, `/<tag>` otherwise.
    pub fn language_prefix(&self, language: &LanguageTag) -> String {
        if language == self.default_language() {
            String::new()
        } else {
            format!("/{}", language.as_str().to_ascii_lowercase())
        }
    }
}
