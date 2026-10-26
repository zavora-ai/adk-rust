//! Launch-gate extensions a site adds by kind or deployment (spec 3.5 §7.8.1, §7.16.1, D-11,
//! A-I5): [`ThirdPartyResources`] for kinds that ship no third-party scripts or trackers, and
//! [`ReleaseDelegation`] for a served site whose release key the owner's root must delegate.

use std::sync::OnceLock;

use adk_awp::signing::keys::{Dated, delegated_key, verify_key_set};
use async_trait::async_trait;
use awp_types::{KeyScope, KeySet, PublicKeyJwk};
use regex::Regex;
use scraper::{Html, Selector};
use url::Url;

use super::{CheckResult, CheckStatus, Finding, GateCheck, GateExtension, GateInput, GateMode};
use crate::error::Result;

/// Fails the gate for any page or stylesheet that loads a resource from another origin: a
/// script, a frame, an image or image set, media, a stylesheet, a preloaded or prefetched
/// file, a font or a CSS import (spec 3.5 §7.16.1 "no third-party scripts or trackers", S16).
/// It reports as [`GateCheck::ThirdPartyResources`] in both tiers, with rule
/// `third-party-resource`.
///
/// # Example
///
/// ```rust
/// use adk_site::gate::ThirdPartyResources;
///
/// let check = ThirdPartyResources::new("https://kaunti.go.ke");
/// assert!(check.third_party("https://tracker.example/t.js"));
/// assert!(check.third_party("//cdn.example/x.css"));
/// assert!(!check.third_party("/assets/site.js"));
/// assert!(!check.third_party("https://kaunti.go.ke/assets/site.js"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThirdPartyResources {
    origin: Option<String>,
}

/// Elements and the attributes that load a resource.
const LOADS: &[(&str, &str)] = &[
    ("script[src]", "src"),
    ("iframe[src]", "src"),
    ("frame[src]", "src"),
    ("img[src]", "src"),
    ("img[srcset]", "srcset"),
    ("source[src]", "src"),
    ("source[srcset]", "srcset"),
    ("video[src]", "src"),
    ("video[poster]", "poster"),
    ("audio[src]", "src"),
    ("track[src]", "src"),
    ("embed[src]", "src"),
    ("object[data]", "data"),
    ("input[type=image][src]", "src"),
    ("link[rel~=stylesheet][href]", "href"),
    ("link[rel~=preload][href]", "href"),
    ("link[rel~=modulepreload][href]", "href"),
    ("link[rel~=prefetch][href]", "href"),
    ("link[rel~=icon][href]", "href"),
    ("link[rel~=manifest][href]", "href"),
];

/// `url(…)` and `@import "…"` in CSS.
fn css_urls(css: &str) -> Vec<String> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        Regex::new(r#"(?i)url\(\s*['"]?([^'")\s]+)['"]?\s*\)|@import\s+['"]([^'"]+)['"]"#)
            .expect("a static regex")
    });
    pattern
        .captures_iter(css)
        .filter_map(|c| c.get(1).or_else(|| c.get(2)).map(|m| m.as_str().to_string()))
        .collect()
}

impl ThirdPartyResources {
    /// The check for a site served at `origin`.
    pub fn new(origin: &str) -> Self {
        Self { origin: Url::parse(origin).ok().map(|url| url.origin().ascii_serialization()) }
    }

    /// Whether `reference` loads from another origin than the site's.
    pub fn third_party(&self, reference: &str) -> bool {
        let reference = reference.trim();
        if reference.starts_with("//") {
            return true;
        }
        match Url::parse(reference) {
            Ok(url) if matches!(url.scheme(), "http" | "https") => {
                self.origin.as_deref() != Some(url.origin().ascii_serialization().as_str())
            }
            Ok(_) | Err(_) => false,
        }
    }

    fn finding(&self, path: &str, what: &str, value: &str) -> Finding {
        Finding::blocking(
            "third-party-resource",
            Some(path.to_string()),
            None,
            format!(
                "{what} loads {value} from another origin; the site ships no third-party scripts \
                 or trackers (spec 3.5 §7.16.1)"
            ),
        )
    }
}

#[async_trait]
impl GateExtension for ThirdPartyResources {
    fn check(&self) -> GateCheck {
        GateCheck::ThirdPartyResources
    }

    fn runs_in(&self, _mode: GateMode) -> bool {
        true
    }

    async fn run(&self, input: &GateInput<'_>) -> Result<CheckResult> {
        let paths = match input.changed {
            Some(_) => &input.report.written,
            None => &input.report.outputs,
        };
        let mut findings = Vec::new();
        for path in paths {
            let full = input.out.join(path.trim_start_matches('/'));
            if path.ends_with(".css") {
                let Ok(css) = tokio::fs::read_to_string(&full).await else { continue };
                for value in css_urls(&css).iter().filter(|v| self.third_party(v)) {
                    findings.push(self.finding(path, "the stylesheet", value));
                }
                continue;
            }
            if !(path.ends_with(".html") || path.ends_with(".htm")) {
                continue;
            }
            let Ok(text) = tokio::fs::read_to_string(&full).await else { continue };
            let document = Html::parse_document(&text);
            for (selector, attribute) in LOADS {
                let selector = Selector::parse(selector).expect("a static selector");
                for element in document.select(&selector) {
                    let value = element.attr(attribute).unwrap_or_default();
                    let references: Vec<&str> = if attribute.ends_with("srcset") {
                        value
                            .split(',')
                            .filter_map(|candidate| candidate.split_whitespace().next())
                            .collect()
                    } else {
                        vec![value]
                    };
                    for reference in references.into_iter().filter(|r| self.third_party(r)) {
                        let what = format!("<{}>", element.value().name());
                        findings.push(self.finding(path, &what, reference));
                    }
                }
            }
            let styles = Selector::parse("style, [style]").expect("a static selector");
            for element in document.select(&styles) {
                let css = if element.value().name() == "style" {
                    element.text().collect::<String>()
                } else {
                    element.attr("style").unwrap_or_default().to_string()
                };
                for value in css_urls(&css).iter().filter(|v| self.third_party(v)) {
                    findings.push(self.finding(path, "an inline style", value));
                }
            }
        }
        let status = if findings.is_empty() { CheckStatus::Passed } else { CheckStatus::Failed };
        Ok(CheckResult {
            check: GateCheck::ThirdPartyResources,
            status,
            findings,
            note: Some("no third-party scripts, trackers or other resources".into()),
        })
    }
}

/// The key set's path in a release.
const KEYS: &str = ".well-known/awp/keys.json";

/// Refuses to publish until the owner's root has delegated the release key the site log signs
/// with (spec 3.5 §7.2, D-11, A-I5): a served site never signs its own contract or
/// delegations. It reports as [`GateCheck::Signing`] in both tiers, with rule
/// `release-delegation`.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_site::gate::{LaunchGate, ReleaseDelegation};
///
/// # fn demo(key: awp_types::PublicKeyJwk) {
/// let gate = LaunchGate::new()
///     .with_extension(Arc::new(ReleaseDelegation::new("release-1", key, "data/keys/release.jwk")));
/// # let _ = gate;
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseDelegation {
    kid: String,
    key: PublicKeyJwk,
    key_file: String,
}

impl ReleaseDelegation {
    /// The check for the release key `kid` with public key `key`, kept at `key_file`.
    pub fn new(kid: impl Into<String>, key: PublicKeyJwk, key_file: impl Into<String>) -> Self {
        Self { kid: kid.into(), key, key_file: key_file.into() }
    }

    /// What is wrong with the key set `keys`, if anything.
    fn problem(&self, keys: Option<&[u8]>) -> Option<String> {
        let Some(bytes) = keys else {
            return Some("the site has no AWP key set".to_string());
        };
        let keys: KeySet = match serde_json::from_slice(bytes) {
            Ok(keys) => keys,
            Err(error) => return Some(format!("its key set is not valid: {error}")),
        };
        if let Err(error) = verify_key_set(&keys) {
            return Some(format!("its key set does not verify: {error}"));
        }
        match delegated_key(&keys, &self.kid, KeyScope::Release, Dated::Live(chrono::Utc::now())) {
            Ok(key) if *key == self.key => None,
            Ok(_) => Some(format!("the key set's `{}` is not the release key", self.kid)),
            Err(error) => Some(format!("the release key `{}` is not delegated: {error}", self.kid)),
        }
    }
}

#[async_trait]
impl GateExtension for ReleaseDelegation {
    fn check(&self) -> GateCheck {
        GateCheck::Signing
    }

    fn runs_in(&self, _mode: GateMode) -> bool {
        true
    }

    async fn run(&self, input: &GateInput<'_>) -> Result<CheckResult> {
        let documents = &input.project.documents;
        let keys = match documents.get(KEYS).or_else(|| documents.get(&format!("/{KEYS}"))) {
            Some(bytes) => Some(bytes.to_vec()),
            None => tokio::fs::read(input.out.join(KEYS)).await.ok(),
        };
        let findings: Vec<Finding> = self
            .problem(keys.as_deref())
            .map(|problem| Finding {
                rule: "release-delegation".into(),
                severity: super::Severity::Critical,
                blocking: true,
                path: Some(format!("/{KEYS}")),
                item: None,
                message: format!(
                    "{problem}. The owner signs at a terminal: `adk-rust awp keys rotate` makes \
                     the root and the key set, then `adk-rust awp keys delegate --scope release \
                     --key {}` delegates the release key (spec 3.5 D-11)",
                    self.key_file
                ),
            })
            .into_iter()
            .collect();
        let status = if findings.is_empty() { CheckStatus::Passed } else { CheckStatus::Failed };
        Ok(CheckResult {
            check: GateCheck::Signing,
            status,
            findings,
            note: Some("the release key is delegated by the owner's root".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_references_are_read() {
        let css = "@import 'https://fonts.example/a.css'; body { background: url(\"/bg.png\") } \
                   h1 { background-image: url( https://t.example/p.gif ) }";
        assert_eq!(
            css_urls(css),
            ["https://fonts.example/a.css", "/bg.png", "https://t.example/p.gif"]
        );
    }
}
