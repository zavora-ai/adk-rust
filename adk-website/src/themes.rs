//! Default themes for the built-in renderer (spec 3.5 §7.16 `themes`, §7.18 stage 6,
//! §7.23 "Themes", §13 Q3).
//!
//! Onboarding offers these as the three design directions of a site without brand assets.
//! Each is an `adk_site::theme::Theme`: the renderer's templates with the theme's design
//! tokens, light by default and dark under `prefers-color-scheme: dark`. Every text pair in
//! both modes meets WCAG 2.2 AA (4.5:1), so a theme never fails the launch gate's contrast
//! check.
//!
//! | Theme | Direction |
//! |---|---|
//! | [`ThemeId::Paper`] | Quiet and typographic: black on white, one blue for links |
//! | [`ThemeId::Harbour`] | Calm and institutional: deep blue on a cool white |
//! | [`ThemeId::Meadow`] | Warm and local: green on cream, rounder corners |
//!
//! A developer's own theme is a directory `adk_site::theme::Theme::load` reads (§7.23).

use std::fmt;
use std::str::FromStr;

use adk_site::theme::{ColorTokens, ContrastPair, DesignTokens, Rgb, Theme};
use serde::{Deserialize, Serialize};

/// A default theme (spec 3.5 §7.16 `themes`).
///
/// # Example
///
/// ```rust
/// use adk_website::themes::ThemeId;
///
/// let id: ThemeId = "harbour".parse().unwrap();
/// assert_eq!(id, ThemeId::Harbour);
/// assert_eq!(ThemeId::ALL.len(), 3);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeId {
    /// Quiet and typographic.
    Paper,
    /// Calm and institutional.
    Harbour,
    /// Warm and local.
    Meadow,
}

impl ThemeId {
    /// Every default theme: the three design directions (spec 3.5 §7.18 stage 6).
    pub const ALL: [ThemeId; 3] = [ThemeId::Paper, ThemeId::Harbour, ThemeId::Meadow];

    /// The theme's name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Paper => "paper",
            Self::Harbour => "harbour",
            Self::Meadow => "meadow",
        }
    }

    /// The design direction in the owner's words, for the onboarding card.
    pub fn direction(self) -> &'static str {
        match self {
            Self::Paper => "Quiet and typographic: black on white with one blue for links",
            Self::Harbour => "Calm and institutional: deep blue on a cool white",
            Self::Meadow => "Warm and local: green on cream with softer corners",
        }
    }
}

impl fmt::Display for ThemeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ThemeId {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|id| id.as_str() == text.trim().to_ascii_lowercase())
            .ok_or_else(|| format!("unknown theme `{text}`; use paper, harbour or meadow"))
    }
}

/// A light or a dark colour scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scheme {
    /// The default.
    Light,
    /// Under `prefers-color-scheme: dark`.
    Dark,
}

/// A theme's tokens in both schemes (spec 3.5 §7.16 `themes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeTokens {
    /// The light tokens, emitted as the stylesheet's `:root` variables.
    pub light: DesignTokens,
    /// The dark colours, emitted under `prefers-color-scheme: dark`.
    pub dark: ColorTokens,
}

impl ThemeTokens {
    /// The tokens of `scheme`.
    pub fn scheme(&self, scheme: Scheme) -> DesignTokens {
        match scheme {
            Scheme::Light => self.light.clone(),
            Scheme::Dark => DesignTokens { colors: self.dark.clone(), ..self.light.clone() },
        }
    }

    /// Every text pair of both schemes with its WCAG 2.2 AA minimum.
    pub fn contrast_pairs(&self) -> Vec<(Scheme, ContrastPair)> {
        [Scheme::Light, Scheme::Dark]
            .into_iter()
            .flat_map(|scheme| {
                self.scheme(scheme).contrast_pairs().into_iter().map(move |pair| (scheme, pair))
            })
            .collect()
    }
}

#[allow(clippy::too_many_arguments)]
fn colors(
    text: &str,
    background: &str,
    muted: &str,
    link: &str,
    accent: &str,
    accent_text: &str,
    border: &str,
    notice_background: &str,
    notice_text: &str,
) -> ColorTokens {
    let rgb = |hex: &str| -> Rgb { hex.parse().expect("a theme colour is #rrggbb") };
    ColorTokens {
        text: rgb(text),
        background: rgb(background),
        muted: rgb(muted),
        link: rgb(link),
        accent: rgb(accent),
        accent_text: rgb(accent_text),
        border: rgb(border),
        notice_background: rgb(notice_background),
        notice_text: rgb(notice_text),
    }
}

const SANS: &str = "system-ui, -apple-system, \"Segoe UI\", Roboto, sans-serif";
const SERIF: &str = "Charter, \"Bitstream Charter\", Georgia, \"Times New Roman\", serif";
const MONO: &str = "ui-monospace, \"SF Mono\", Menlo, Consolas, monospace";

/// The tokens of `id`.
///
/// # Example
///
/// ```rust
/// use adk_website::themes::{ThemeId, tokens};
///
/// assert!(tokens(ThemeId::Meadow).contrast_pairs().iter().all(|(_, pair)| pair.passes()));
/// ```
pub fn tokens(id: ThemeId) -> ThemeTokens {
    let (light, dark, body, heading, measure, radius) = match id {
        ThemeId::Paper => (
            colors(
                "#1b1d21", "#ffffff", "#555b65", "#0b57d0", "#1b1d21", "#ffffff", "#d0d4da",
                "#fff4d6", "#4a3500",
            ),
            colors(
                "#e8eaed", "#16181c", "#a7adb6", "#8ab4f8", "#e8eaed", "#16181c", "#33373d",
                "#3d3000", "#fde7a6",
            ),
            SERIF,
            SANS,
            "42rem",
            "2px",
        ),
        ThemeId::Harbour => (
            colors(
                "#0f2433", "#f7fbfd", "#3d5566", "#0b5d8a", "#0b5d8a", "#ffffff", "#c9d8e2",
                "#fff1d6", "#5a3b00",
            ),
            colors(
                "#e6f0f5", "#0d1b24", "#9fb4c2", "#7cc4ea", "#7cc4ea", "#0d1b24", "#2a3d4a",
                "#3b2a05", "#ffe2a8",
            ),
            SANS,
            SANS,
            "46rem",
            "4px",
        ),
        ThemeId::Meadow => (
            colors(
                "#1e2a1e", "#fbfaf5", "#4f5d4f", "#2f6b2f", "#2f6b2f", "#ffffff", "#d6dccb",
                "#fdf0d5", "#573b00",
            ),
            colors(
                "#ecf1e6", "#141a12", "#a9b7a0", "#9ed39a", "#9ed39a", "#141a12", "#2f3a2b",
                "#3a2b08", "#ffe6ad",
            ),
            SANS,
            SERIF,
            "44rem",
            "10px",
        ),
    };
    ThemeTokens {
        light: DesignTokens {
            colors: light,
            font_body: body.into(),
            font_heading: heading.into(),
            font_mono: MONO.into(),
            measure: measure.into(),
            radius: radius.into(),
        },
        dark,
    }
}

/// The theme `id` for the built-in renderer: the default templates and stylesheet with the
/// theme's tokens, `color-scheme: light dark`, and its dark scheme (spec 3.5 §7.16).
///
/// # Example
///
/// ```rust
/// use adk_website::themes::{ThemeId, theme};
///
/// let harbour = theme(ThemeId::Harbour);
/// assert_eq!(harbour.name, "harbour");
/// assert!(harbour.stylesheet().contains("prefers-color-scheme: dark"));
/// ```
pub fn theme(id: ThemeId) -> Theme {
    let tokens = tokens(id);
    let base = Theme::default();
    let dark = &tokens.dark;
    let mut css = String::from(
        ":root { color-scheme: light dark; }\n@media (prefers-color-scheme: dark) {\n  :root {\n",
    );
    for (name, value) in [
        ("text", dark.text),
        ("background", dark.background),
        ("muted", dark.muted),
        ("link", dark.link),
        ("accent", dark.accent),
        ("accent-text", dark.accent_text),
        ("border", dark.border),
        ("notice-background", dark.notice_background),
        ("notice-text", dark.notice_text),
    ] {
        css.push_str(&format!("    --color-{name}: {value};\n"));
    }
    css.push_str("  }\n}\n");
    css.push_str(&base.css);
    Theme { name: id.as_str().to_string(), tokens: tokens.light, css, ..base }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_text_pair_passes_wcag_aa_in_both_schemes() {
        for id in ThemeId::ALL {
            for (scheme, pair) in tokens(id).contrast_pairs() {
                assert!(pair.passes(), "{id} {scheme:?} {}: {:.2}", pair.name, pair.ratio);
            }
        }
    }

    #[test]
    fn the_stylesheet_carries_both_schemes_and_the_renderer_rules() {
        for id in ThemeId::ALL {
            let theme = theme(id);
            let css = theme.stylesheet();
            assert!(css.starts_with(":root {"), "{id}");
            assert!(css.contains(&format!("--color-text: {}", tokens(id).light.colors.text)));
            assert!(css.contains(&format!("--color-text: {}", tokens(id).dark.text)));
            assert!(css.contains(".skip-link"), "the renderer's rules stay");
            assert_eq!(theme.templates, Theme::default().templates);
        }
    }

    #[test]
    fn names_parse_back() {
        for id in ThemeId::ALL {
            assert_eq!(id.as_str().parse::<ThemeId>().unwrap(), id);
        }
        assert!("neon".parse::<ThemeId>().is_err());
    }
}
