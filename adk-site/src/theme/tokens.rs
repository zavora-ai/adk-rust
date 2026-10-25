//! Design tokens and their colour contrast (spec 3.5 §7.8 "Generator", §7.8.1
//! "Accessibility").

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// An sRGB colour, written `#rrggbb` or `#rgb`.
///
/// # Example
///
/// ```rust
/// use adk_site::theme::Rgb;
///
/// let ink: Rgb = "#1a1a1a".parse().unwrap();
/// let paper: Rgb = "#fff".parse().unwrap();
/// assert!(ink.contrast_ratio(paper) > 15.0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// The WCAG 2.2 relative luminance.
    pub fn relative_luminance(self) -> f64 {
        fn channel(value: u8) -> f64 {
            let c = f64::from(value) / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        }
        0.2126 * channel(self.0) + 0.7152 * channel(self.1) + 0.0722 * channel(self.2)
    }

    /// The WCAG 2.2 contrast ratio against `other`, from 1 to 21.
    pub fn contrast_ratio(self, other: Rgb) -> f64 {
        let (a, b) = (self.relative_luminance(), other.relative_luminance());
        let (light, dark) = if a >= b { (a, b) } else { (b, a) };
        (light + 0.05) / (dark + 0.05)
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

impl FromStr for Rgb {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let hex = text
            .trim()
            .strip_prefix('#')
            .ok_or_else(|| format!("colour '{text}' must start with '#'"))?;
        let digits: Vec<u8> = hex
            .chars()
            .map(|c| c.to_digit(16).map(|d| d as u8))
            .collect::<Option<_>>()
            .ok_or_else(|| format!("colour '{text}' is not hexadecimal"))?;
        match digits.as_slice() {
            [r, g, b] => Ok(Self(r * 17, g * 17, b * 17)),
            [r1, r2, g1, g2, b1, b2] => Ok(Self(r1 * 16 + r2, g1 * 16 + g2, b1 * 16 + b2)),
            _ => Err(format!("colour '{text}' must be #rgb or #rrggbb")),
        }
    }
}

impl Serialize for Rgb {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?.parse().map_err(serde::de::Error::custom)
    }
}

/// The colour roles a theme defines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ColorTokens {
    /// Body text.
    pub text: Rgb,
    /// Page background.
    pub background: Rgb,
    /// Secondary text: dates, bylines, captions.
    pub muted: Rgb,
    /// Links.
    pub link: Rgb,
    /// Buttons and highlights.
    pub accent: Rgb,
    /// Text on the accent colour.
    pub accent_text: Rgb,
    /// Rules and outlines.
    pub border: Rgb,
    /// Correction, withdrawal and expiry notices: background.
    pub notice_background: Rgb,
    /// Correction, withdrawal and expiry notices: text.
    pub notice_text: Rgb,
}

/// A theme's design tokens: colours, type and measure, emitted as CSS custom properties.
///
/// # Example
///
/// ```rust
/// use adk_site::theme::DesignTokens;
///
/// let tokens = DesignTokens::default();
/// assert!(tokens.contrast_pairs().iter().all(|pair| pair.passes()));
/// assert!(tokens.css_variables().contains("--color-text: #"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DesignTokens {
    /// Colours.
    pub colors: ColorTokens,
    /// The body font stack.
    pub font_body: String,
    /// The heading font stack.
    pub font_heading: String,
    /// The monospace font stack.
    pub font_mono: String,
    /// The maximum width of the reading column, a CSS length.
    pub measure: String,
    /// The corner radius, a CSS length.
    pub radius: String,
}

impl Default for DesignTokens {
    fn default() -> Self {
        Self {
            colors: ColorTokens {
                text: Rgb(0x1b, 0x1d, 0x21),
                background: Rgb(0xff, 0xff, 0xff),
                muted: Rgb(0x55, 0x5b, 0x65),
                link: Rgb(0x0b, 0x57, 0xd0),
                accent: Rgb(0x0b, 0x57, 0xd0),
                accent_text: Rgb(0xff, 0xff, 0xff),
                border: Rgb(0xd0, 0xd4, 0xda),
                notice_background: Rgb(0xff, 0xf4, 0xd6),
                notice_text: Rgb(0x4a, 0x35, 0x00),
            },
            font_body: "system-ui, -apple-system, \"Segoe UI\", Roboto, sans-serif".into(),
            font_heading: "system-ui, -apple-system, \"Segoe UI\", Roboto, sans-serif".into(),
            font_mono: "ui-monospace, \"SF Mono\", Menlo, Consolas, monospace".into(),
            measure: "44rem".into(),
            radius: "6px".into(),
        }
    }
}

/// A foreground and background pair the accessibility check measures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContrastPair {
    /// The pair's name, such as `text/background`.
    pub name: String,
    /// The foreground colour.
    pub foreground: Rgb,
    /// The background colour.
    pub background: Rgb,
    /// The measured ratio.
    pub ratio: f64,
    /// The WCAG 2.2 AA minimum for the pair: 4.5 for text.
    pub required: f64,
}

impl ContrastPair {
    /// Whether the pair meets its minimum.
    pub fn passes(&self) -> bool {
        self.ratio + 1e-9 >= self.required
    }
}

impl DesignTokens {
    /// Every text pair the theme renders, with its WCAG 2.2 AA minimum (1.4.3: 4.5:1 for
    /// normal text).
    pub fn contrast_pairs(&self) -> Vec<ContrastPair> {
        let c = &self.colors;
        [
            ("text/background", c.text, c.background),
            ("muted/background", c.muted, c.background),
            ("link/background", c.link, c.background),
            ("accent_text/accent", c.accent_text, c.accent),
            ("notice_text/notice_background", c.notice_text, c.notice_background),
        ]
        .into_iter()
        .map(|(name, foreground, background)| ContrastPair {
            name: name.into(),
            foreground,
            background,
            ratio: foreground.contrast_ratio(background),
            required: 4.5,
        })
        .collect()
    }

    /// The tokens as a `:root` block of CSS custom properties.
    pub fn css_variables(&self) -> String {
        let c = &self.colors;
        let mut css = String::from(":root {\n");
        for (name, value) in [
            ("text", c.text),
            ("background", c.background),
            ("muted", c.muted),
            ("link", c.link),
            ("accent", c.accent),
            ("accent-text", c.accent_text),
            ("border", c.border),
            ("notice-background", c.notice_background),
            ("notice-text", c.notice_text),
        ] {
            css.push_str(&format!("  --color-{name}: {value};\n"));
        }
        css.push_str(&format!("  --font-body: {};\n", self.font_body));
        css.push_str(&format!("  --font-heading: {};\n", self.font_heading));
        css.push_str(&format!("  --font-mono: {};\n", self.font_mono));
        css.push_str(&format!("  --measure: {};\n", self.measure));
        css.push_str(&format!("  --radius: {};\n", self.radius));
        css.push_str("}\n");
        css
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_matches_wcag_reference_values() {
        let black = Rgb(0, 0, 0);
        let white = Rgb(255, 255, 255);
        assert!((black.contrast_ratio(white) - 21.0).abs() < 1e-9);
        assert!((white.contrast_ratio(white) - 1.0).abs() < 1e-9);
        // #767676 on white is the classic 4.54:1 boundary colour.
        let grey: Rgb = "#767676".parse().unwrap();
        assert!((grey.contrast_ratio(white) - 4.54).abs() < 0.01);
        let light: Rgb = "#777".parse().unwrap();
        assert!(light.contrast_ratio(white) < 4.5);
    }

    #[test]
    fn colours_parse_and_print() {
        assert_eq!("#0B57D0".parse::<Rgb>().unwrap(), Rgb(0x0b, 0x57, 0xd0));
        assert_eq!("#abc".parse::<Rgb>().unwrap(), Rgb(0xaa, 0xbb, 0xcc));
        assert_eq!(Rgb(1, 2, 255).to_string(), "#0102ff");
        assert!("0b57d0".parse::<Rgb>().is_err());
        assert!("#12345".parse::<Rgb>().is_err());
    }

    #[test]
    fn default_tokens_meet_aa() {
        for pair in DesignTokens::default().contrast_pairs() {
            assert!(pair.passes(), "{} is {:.2}", pair.name, pair.ratio);
        }
    }
}
