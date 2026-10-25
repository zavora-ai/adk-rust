//! Themes for the built-in renderer: templates and design tokens (spec 3.5 §7.8 "Generator",
//! §7.16 `themes`, §7.23 "Themes").
//!
//! A theme is a directory:
//!
//! | File | Holds |
//! |------|-------|
//! | `theme.toml` | `name`, and `[tokens]` overriding [`DesignTokens`] |
//! | `templates/*.html` | [minijinja](https://docs.rs/minijinja) templates replacing `base.html`, `item.html`, `list.html` or `notice.html` |
//! | `style.css` | Rules appended after the token variables, replacing the default stylesheet |
//!
//! Every template is rendered with HTML auto-escaping; Markdown bodies arrive with raw HTML
//! escaped, so no theme or content can put script on a page except through its templates.

mod defaults;
mod tokens;

use std::collections::BTreeMap;
use std::path::Path;

use minijinja::Environment;
use serde::{Deserialize, Serialize};

use crate::error::{Result, SiteError};

pub use tokens::{ColorTokens, ContrastPair, DesignTokens, Rgb};

/// The templates a theme provides.
pub const TEMPLATE_NAMES: &[&str] = &["base.html", "item.html", "list.html", "notice.html"];

/// A theme: templates, design tokens, a stylesheet and interface strings.
///
/// # Example
///
/// ```rust
/// use adk_site::theme::Theme;
///
/// let theme = Theme::default();
/// assert!(theme.stylesheet().starts_with(":root {"));
/// assert_eq!(theme.string("skip_to_content"), "Skip to content");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// The theme's name, recorded in build reports.
    pub name: String,
    /// The design tokens.
    pub tokens: DesignTokens,
    /// Template sources by name.
    pub templates: BTreeMap<String, String>,
    /// Stylesheet rules after the token variables.
    pub css: String,
    /// Interface strings in English.
    pub strings: BTreeMap<String, String>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "default".into(),
            tokens: DesignTokens::default(),
            templates: [
                ("base.html", defaults::BASE),
                ("item.html", defaults::ITEM),
                ("list.html", defaults::LIST),
                ("notice.html", defaults::NOTICE),
            ]
            .into_iter()
            .map(|(name, source)| (name.to_string(), source.to_string()))
            .collect(),
            css: defaults::STYLESHEET.to_string(),
            strings: defaults::STRINGS
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    tokens: Option<DesignTokens>,
    #[serde(default)]
    strings: BTreeMap<String, String>,
}

impl Theme {
    /// Loads a theme directory over the default theme: files it provides replace the
    /// defaults.
    ///
    /// # Errors
    ///
    /// [`SiteError::Io`] when a file cannot be read; [`SiteError::Template`] when
    /// `theme.toml` is invalid or a template does not compile.
    pub fn load(dir: &Path) -> Result<Self> {
        let mut theme = Self::default();
        let manifest = dir.join("theme.toml");
        if manifest.exists() {
            let text =
                std::fs::read_to_string(&manifest).map_err(|e| SiteError::io(&manifest, e))?;
            let file: ThemeFile = toml::from_str(&text).map_err(|e| SiteError::Template {
                template: "theme.toml".into(),
                message: e.to_string(),
            })?;
            if let Some(name) = file.name {
                theme.name = name;
            }
            if let Some(tokens) = file.tokens {
                theme.tokens = tokens;
            }
            theme.strings.extend(file.strings);
        }
        for name in TEMPLATE_NAMES {
            let path = dir.join("templates").join(name);
            if path.exists() {
                let source = std::fs::read_to_string(&path).map_err(|e| SiteError::io(&path, e))?;
                theme.templates.insert((*name).to_string(), source);
            }
        }
        let css = dir.join("style.css");
        if css.exists() {
            theme.css = std::fs::read_to_string(&css).map_err(|e| SiteError::io(&css, e))?;
        }
        theme.environment()?;
        Ok(theme)
    }

    /// The stylesheet: token variables, then the theme's rules.
    pub fn stylesheet(&self) -> String {
        format!("{}{}", self.tokens.css_variables(), self.css)
    }

    /// An interface string in English, or the key when the theme has none.
    pub fn string<'a>(&'a self, key: &'a str) -> &'a str {
        self.strings.get(key).map_or(key, String::as_str)
    }

    /// The template environment with every template compiled.
    ///
    /// # Errors
    ///
    /// [`SiteError::Template`] when a template does not compile.
    pub(crate) fn environment(&self) -> Result<Environment<'static>> {
        let mut env = Environment::new();
        env.set_auto_escape_callback(|_| minijinja::AutoEscape::Html);
        env.set_undefined_behavior(minijinja::UndefinedBehavior::Lenient);
        // Escapes the five HTML metacharacters and renders `none` as nothing; minijinja's
        // default also encodes `/`, which makes every URL unreadable in the source.
        env.set_formatter(|out, state, value| {
            if value.is_undefined() || value.is_none() {
                return Ok(());
            }
            let text = value.to_string();
            if value.is_safe() || matches!(state.auto_escape(), minijinja::AutoEscape::None) {
                return Ok(std::fmt::Write::write_str(out, &text)?);
            }
            let escaped = text
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&#39;");
            Ok(std::fmt::Write::write_str(out, &escaped)?)
        });
        for (name, source) in &self.templates {
            env.add_template_owned(name.clone(), source.clone()).map_err(|e| {
                SiteError::Template { template: name.clone(), message: e.to_string() }
            })?;
        }
        Ok(env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_templates_compile() {
        let env = Theme::default().environment().unwrap();
        for name in TEMPLATE_NAMES {
            assert!(env.get_template(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn a_theme_directory_overrides_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("templates")).unwrap();
        std::fs::write(
            dir.path().join("theme.toml"),
            "name = \"night\"\n[strings]\ntags = \"Topics\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("style.css"), "body { margin: 2rem; }\n").unwrap();
        std::fs::write(dir.path().join("templates/notice.html"), "{% extends \"base.html\" %}")
            .unwrap();
        let theme = Theme::load(dir.path()).unwrap();
        assert_eq!(theme.name, "night");
        assert_eq!(theme.string("tags"), "Topics");
        assert!(theme.stylesheet().ends_with("body { margin: 2rem; }\n"));
        assert_eq!(theme.templates["notice.html"], "{% extends \"base.html\" %}");

        std::fs::write(dir.path().join("templates/list.html"), "{% if %}").unwrap();
        assert!(matches!(Theme::load(dir.path()), Err(SiteError::Template { .. })));
    }
}
