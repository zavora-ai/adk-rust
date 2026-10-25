//! The third-party script check a public body's launch gate adds (spec 3.5 §7.16.1
//! Government: "no third-party scripts or trackers"; S16).

use adk_site::gate::{
    CheckResult, CheckStatus, Finding, GateCheck, GateExtension, GateInput, GateMode, Severity,
};
use async_trait::async_trait;
use scraper::{Html, Selector};
use url::Url;

/// Fails the gate for any page that loads a script, a frame, a stylesheet or an image from
/// another origin (spec 3.5 §7.16.1, S16). It reports under the charter's red lines, which
/// name the rule, in both tiers.
///
/// # Example
///
/// ```rust
/// use adk_website::kinds::ThirdPartyScripts;
///
/// let check = ThirdPartyScripts::new("https://kaunti.go.ke");
/// assert!(check.third_party("https://tracker.example/t.js"));
/// assert!(!check.third_party("/assets/site.js"));
/// assert!(!check.third_party("https://kaunti.go.ke/assets/site.js"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThirdPartyScripts {
    origin: Option<String>,
}

impl ThirdPartyScripts {
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
}

#[async_trait]
impl GateExtension for ThirdPartyScripts {
    fn check(&self) -> GateCheck {
        GateCheck::RedLines
    }

    fn runs_in(&self, _mode: GateMode) -> bool {
        true
    }

    async fn run(&self, input: &GateInput<'_>) -> adk_site::Result<CheckResult> {
        let paths = match input.changed {
            Some(_) => &input.report.written,
            None => &input.report.outputs,
        };
        let selector = Selector::parse(
            "script[src], iframe[src], img[src], link[rel~=stylesheet][href], link[rel~=preload][href]",
        )
        .expect("a static selector");
        let mut findings = Vec::new();
        for path in paths.iter().filter(|path| path.ends_with(".html")) {
            let full = input.out.join(path.trim_start_matches('/'));
            let Ok(text) = tokio::fs::read_to_string(&full).await else { continue };
            let document = Html::parse_document(&text);
            for element in document.select(&selector) {
                let value =
                    element.attr("src").or_else(|| element.attr("href")).unwrap_or_default();
                if self.third_party(value) {
                    findings.push(Finding {
                        rule: "third-party-resource".into(),
                        severity: Severity::Serious,
                        blocking: true,
                        path: Some(path.clone()),
                        item: None,
                        message: format!(
                            "<{}> loads {value} from another origin; a public body ships no \
                             third-party scripts or trackers (spec 3.5 §7.16.1)",
                            element.value().name()
                        ),
                    });
                }
            }
        }
        let status = if findings.is_empty() { CheckStatus::Passed } else { CheckStatus::Failed };
        Ok(CheckResult {
            check: GateCheck::RedLines,
            status,
            findings,
            note: Some("no third-party scripts or trackers".into()),
        })
    }
}
