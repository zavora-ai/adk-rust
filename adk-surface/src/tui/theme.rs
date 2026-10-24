//! Light and dark from the visual tokens of spec 3.3 §9.

use std::str::FromStr;
use std::time::Duration;

use ratatui::style::{Color, Modifier, Style};

/// Which theme the terminal uses (spec 3.3 §9): detected from the terminal's background,
/// or set.
///
/// # Example
///
/// ```rust
/// use adk_surface::tui::ThemeChoice;
///
/// assert_eq!("dark".parse::<ThemeChoice>().unwrap(), ThemeChoice::Dark);
/// assert_eq!(ThemeChoice::default(), ThemeChoice::Auto);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    /// Light on a light terminal background, dark on a dark one.
    #[default]
    Auto,
    /// The light set.
    Light,
    /// The dark set.
    Dark,
}

impl FromStr for ThemeChoice {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "light" => Ok(Self::Light),
            "dark" => Ok(Self::Dark),
            other => Err(format!("unknown theme '{other}': use auto, light or dark")),
        }
    }
}

/// Light or dark: which set of the spec 3.3 §9 tokens the terminal draws with.
///
/// # Example
///
/// ```rust
/// use adk_surface::tui::{Appearance, ThemeChoice};
///
/// assert_eq!(ThemeChoice::Light.resolve(), Appearance::Light);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    /// The light set of §9.
    Light,
    /// The dark set of §9.
    Dark,
}

impl ThemeChoice {
    /// The appearance: `Auto` asks the terminal for its background (OSC 11, a short wait),
    /// then reads `COLORFGBG`, and falls back to dark.
    pub fn resolve(self) -> Appearance {
        match self {
            Self::Light => Appearance::Light,
            Self::Dark => Appearance::Dark,
            Self::Auto => detect(),
        }
    }
}

fn detect() -> Appearance {
    let mut options = terminal_colorsaurus::QueryOptions::default();
    options.timeout = Duration::from_millis(300);
    match terminal_colorsaurus::theme_mode(options) {
        Ok(terminal_colorsaurus::ThemeMode::Light) => return Appearance::Light,
        Ok(terminal_colorsaurus::ThemeMode::Dark) => return Appearance::Dark,
        Err(error) => tracing::debug!(error = %error, "the terminal did not report its background"),
    }
    // rxvt, Konsole and others: "fg;bg", where 7 and 15 are light backgrounds.
    if let Ok(value) = std::env::var("COLORFGBG")
        && let Some(background) = value.rsplit(';').next()
    {
        return match background.trim() {
            "7" | "15" => Appearance::Light,
            _ => Appearance::Dark,
        };
    }
    Appearance::Dark
}

/// The §9 tokens and the status colours, as RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Palette {
    pub background: (u8, u8, u8),
    pub raised: (u8, u8, u8),
    pub border: (u8, u8, u8),
    pub text: (u8, u8, u8),
    pub secondary: (u8, u8, u8),
    pub label: (u8, u8, u8),
    pub brand: (u8, u8, u8),
    pub accent: (u8, u8, u8),
    pub button: (u8, u8, u8),
    pub button_text: (u8, u8, u8),
    pub decision: (u8, u8, u8),
    /// The decision's header row and the status line: the decision background, deeper.
    pub decision_strong: (u8, u8, u8),
    pub stop: (u8, u8, u8),
    pub stop_text: (u8, u8, u8),
    pub working: (u8, u8, u8),
    pub done: (u8, u8, u8),
    pub failed: (u8, u8, u8),
}

const fn rgb(hex: u32) -> (u8, u8, u8) {
    (((hex >> 16) & 0xff) as u8, ((hex >> 8) & 0xff) as u8, (hex & 0xff) as u8)
}

/// Spec 3.3 §9, light.
const LIGHT: Palette = Palette {
    background: rgb(0xFFFFFF),
    raised: rgb(0xF6F7F9),
    border: rgb(0xE6E8EC),
    text: rgb(0x0F172A),
    secondary: rgb(0x475569),
    label: rgb(0x64748B),
    brand: rgb(0xF74C00),
    accent: rgb(0xC03A00),
    button: rgb(0xC03A00),
    button_text: rgb(0xFFFFFF),
    decision: rgb(0xFFF8F3),
    decision_strong: rgb(0xFFF1EA),
    stop: rgb(0x0A1628),
    stop_text: rgb(0xF1F5F9),
    working: rgb(0x15803D),
    done: rgb(0x1D4ED8),
    failed: rgb(0xB91C1C),
};

/// Spec 3.3 §9, dark: the light screen with the dark set.
const DARK: Palette = Palette {
    background: rgb(0x0A1628),
    raised: rgb(0x1C2B45),
    border: rgb(0x2A3D5C),
    text: rgb(0xF1F5F9),
    secondary: rgb(0xD5DEE8),
    label: rgb(0xA7B4C6),
    brand: rgb(0xF74C00),
    accent: rgb(0xFF9C78),
    button: rgb(0xF74C00),
    button_text: rgb(0x0A1628),
    decision: rgb(0x2A2432),
    decision_strong: rgb(0x352B3F),
    stop: rgb(0xF1F5F9),
    stop_text: rgb(0x0A1628),
    working: rgb(0x4ADE80),
    done: rgb(0x93C5FD),
    failed: rgb(0xFCA5A5),
};

/// The terminal's theme: the spec 3.3 §9 tokens of one appearance, as the terminal can show
/// them.
///
/// # Example
///
/// ```rust
/// use adk_surface::tui::{Appearance, Theme};
///
/// let theme = Theme::new(Appearance::Dark, true);
/// assert_eq!(theme.appearance(), Appearance::Dark);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    appearance: Appearance,
    truecolor: bool,
    palette: Palette,
}

impl Theme {
    /// The theme of `appearance`, in 24-bit colour when `truecolor`, else the nearest of the
    /// 256 indexed colours.
    pub fn new(appearance: Appearance, truecolor: bool) -> Self {
        let palette = match appearance {
            Appearance::Light => LIGHT,
            Appearance::Dark => DARK,
        };
        Self { appearance, truecolor, palette }
    }

    /// The theme of `choice`, in the colours this terminal advertises (`COLORTERM`).
    pub fn detect(choice: ThemeChoice) -> Self {
        Self::new(choice.resolve(), truecolor())
    }

    /// Light or dark.
    pub fn appearance(&self) -> Appearance {
        self.appearance
    }

    pub(crate) fn palette(&self) -> &Palette {
        &self.palette
    }

    pub(crate) fn color(&self, (r, g, b): (u8, u8, u8)) -> Color {
        if self.truecolor { Color::Rgb(r, g, b) } else { Color::Indexed(xterm_index(r, g, b)) }
    }

    /// Text on the page.
    pub(crate) fn base(&self) -> Style {
        Style::new().fg(self.color(self.palette.text)).bg(self.color(self.palette.background))
    }

    pub(crate) fn fg(&self, token: (u8, u8, u8)) -> Style {
        Style::new().fg(self.color(token))
    }

    pub(crate) fn text(&self) -> Style {
        self.fg(self.palette.text)
    }

    pub(crate) fn secondary(&self) -> Style {
        self.fg(self.palette.secondary)
    }

    pub(crate) fn label(&self) -> Style {
        self.fg(self.palette.label)
    }

    pub(crate) fn heading(&self) -> Style {
        self.label().add_modifier(Modifier::BOLD)
    }

    pub(crate) fn accent(&self) -> Style {
        self.fg(self.palette.accent)
    }

    pub(crate) fn border(&self) -> Style {
        self.fg(self.palette.border)
    }

    /// A key, drawn as a keycap.
    pub(crate) fn keycap(&self) -> Style {
        Style::new().fg(self.color(self.palette.background)).bg(self.color(self.palette.text))
    }

    /// The filled badge of a count (spec 3.3 §7.1).
    pub(crate) fn badge(&self) -> Style {
        Style::new()
            .fg(self.color(self.palette.button_text))
            .bg(self.color(self.palette.button))
            .add_modifier(Modifier::BOLD)
    }

    /// The stop control (spec 3.3 §9).
    pub(crate) fn stop(&self) -> Style {
        Style::new().fg(self.color(self.palette.stop_text)).bg(self.color(self.palette.stop))
    }
}

/// Whether the terminal advertises 24-bit colour.
fn truecolor() -> bool {
    let colorterm = std::env::var("COLORTERM").unwrap_or_default().to_ascii_lowercase();
    colorterm == "truecolor"
        || colorterm == "24bit"
        || std::env::var_os("WT_SESSION").is_some()
        || matches!(
            std::env::var("TERM_PROGRAM").as_deref(),
            Ok("iTerm.app" | "WezTerm" | "vscode" | "ghostty")
        )
}

/// The nearest xterm 256-colour index to `(r, g, b)`: the 6×6×6 cube or the grey ramp.
fn xterm_index(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |v: u8| -> usize {
        LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, l)| (i32::from(**l) - i32::from(v)).abs())
            .map_or(0, |(i, _)| i)
    };
    let (ri, gi, bi) = (level(r), level(g), level(b));
    let cube = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    let grey_step = ((u32::from(r) + u32::from(g) + u32::from(b)) / 3).saturating_sub(8) / 10;
    let grey_step = grey_step.min(23) as u8;
    let grey = 8 + 10 * grey_step;
    let distance = |(a, b2, c): (u8, u8, u8)| -> i32 {
        let d = |x: u8, y: u8| (i32::from(x) - i32::from(y)).pow(2);
        d(a, r) + d(b2, g) + d(c, b)
    };
    if distance((grey, grey, grey)) < distance(cube) {
        232 + grey_step
    } else {
        16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tokens_of_section_9() {
        assert_eq!(LIGHT.background, (0xFF, 0xFF, 0xFF));
        assert_eq!(LIGHT.decision, (0xFF, 0xF8, 0xF3));
        assert_eq!(DARK.background, (0x0A, 0x16, 0x28));
        assert_eq!(DARK.decision, (0x2A, 0x24, 0x32));
        assert_eq!(DARK.accent, (0xFF, 0x9C, 0x78));
        assert_eq!(LIGHT.brand, DARK.brand);
    }

    #[test]
    fn indexed_colours_are_the_nearest() {
        assert_eq!(xterm_index(255, 255, 255), 231);
        assert_eq!(xterm_index(0, 0, 0), 16);
        assert_eq!(xterm_index(0x80, 0x80, 0x80), 244);
        let theme = Theme::new(Appearance::Light, false);
        assert!(matches!(theme.color(LIGHT.accent), Color::Indexed(_)));
        assert_eq!(
            Theme::new(Appearance::Dark, true).color(DARK.text),
            Color::Rgb(0xF1, 0xF5, 0xF9)
        );
    }
}
