//! The status line as the tray icon needs it, and the badge it shows (spec 3.3 §5.5, §7.1).

use serde::Deserialize;

/// The part of the engine's `StatusLine` (`GET /api/surface/status?surface=tray`) the tray
/// icon shows. Mode, spend and quiet time are optional, as there; fields it does not know are
/// ignored, so a newer kernel still parses.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayStatus {
    /// Decisions waiting for the caller: the filled badge.
    pub decisions: u32,
    /// Everything is stopped.
    #[serde(default)]
    pub stopped: bool,
    /// The mode, when the kernel runs a harness.
    #[serde(default)]
    pub mode: Option<ModeView>,
    /// Today's spend against the daily limit, when the kernel knows it.
    #[serde(default)]
    pub spend: Option<SpendView>,
    /// Quiet time, when set.
    #[serde(default)]
    pub quiet: Option<QuietView>,
}

/// The mode with its words ("Asks first").
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeView {
    /// As a status: "Asks first".
    pub status: String,
}

/// Today's spend (spec 3.3 §8.1).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendView {
    /// Spent today, micro-USD.
    pub spent_micro_usd: u64,
    /// The daily spending limit, micro-USD, when set.
    #[serde(default)]
    pub limit_micro_usd: Option<u64>,
    /// "$1.84 of $5.00 today".
    pub text: String,
}

/// Quiet time (spec 3.3 §8.1).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuietView {
    /// Quiet time holds now.
    #[serde(default)]
    pub active: bool,
    /// "Quiet from 22:00".
    pub text: String,
}

/// What the tray icon shows (spec 3.3 §5.5, §7.1: counts of waiting decisions are a filled
/// badge on every surface).
///
/// # Example
///
/// ```
/// use adk_desktop_core::{Badge, TrayStatus};
///
/// let waiting = |decisions| TrayStatus { decisions, ..TrayStatus::default() };
/// assert_eq!(Badge::from_status(&waiting(0)), Badge::Idle);
/// assert_eq!(Badge::from_status(&waiting(10)), Badge::Many);
/// assert_eq!(Badge::Many.text(), "9+");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Badge {
    /// Running, nothing waits.
    Idle,
    /// One to nine decisions wait.
    Count(u8),
    /// More than nine decisions wait.
    Many,
    /// Every agent is stopped.
    Stopped,
    /// The kernel cannot be reached.
    Unreachable,
    /// The kernel needs its token.
    SignIn,
}

impl Badge {
    /// The badge for a status read: stopped wins over the count.
    pub fn from_status(status: &TrayStatus) -> Self {
        match status.decisions {
            _ if status.stopped => Self::Stopped,
            0 => Self::Idle,
            count @ 1..=9 => Self::Count(count as u8),
            _ => Self::Many,
        }
    }

    /// The badge's own words, beside the icon on macOS: "2", "9+", or nothing.
    pub fn text(self) -> &'static str {
        const DIGITS: [&str; 10] = ["", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
        match self {
            Self::Count(count) => DIGITS[usize::from(count.min(9))],
            Self::Many => "9+",
            Self::Idle | Self::Stopped | Self::Unreachable | Self::SignIn => "",
        }
    }

    /// The icon variant's file stem under `app/icons/tray/`.
    pub fn icon(self) -> &'static str {
        const COUNTS: [&str; 10] = ["idle", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
        match self {
            Self::Idle => "idle",
            Self::Count(count) => COUNTS[usize::from(count.min(9))],
            Self::Many => "9plus",
            Self::Stopped => "stopped",
            Self::Unreachable => "unreachable",
            Self::SignIn => "signin",
        }
    }

    /// The tooltip, in the surfaces' words (spec 3.3 §8).
    pub fn tooltip(self) -> String {
        match self {
            Self::Idle => "ADK-Rust · Running".to_owned(),
            Self::Count(1) => "ADK-Rust · 1 decision waits for you".to_owned(),
            Self::Count(count) => format!("ADK-Rust · {count} decisions wait for you"),
            Self::Many => "ADK-Rust · more than 9 decisions wait for you".to_owned(),
            Self::Stopped => "ADK-Rust · All agents stopped".to_owned(),
            Self::Unreachable => "ADK-Rust · Can't reach your agents".to_owned(),
            Self::SignIn => "ADK-Rust · Start the tray with --token".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `GET /api/surface/status?surface=tray` from `scripted_tray`, as captured.
    const CAPTURED: &str = r#"{"decisions":2,"hostUtcOffsetMinutes":180,"mode":{"label":"Ask first","mode":"askFirst","status":"Asks first"},"quiet":{"active":false,"end":"07:00","start":"22:00","text":"Quiet from 22:00"},"spend":{"limitMicroUsd":5000000,"spentMicroUsd":0,"text":"$0.00 of $5.00 today"},"stopped":false}"#;

    #[test]
    fn a_captured_status_line_parses_with_and_without_spend_and_quiet() {
        let status: TrayStatus = serde_json::from_str(CAPTURED).unwrap();
        assert_eq!(
            status,
            TrayStatus {
                decisions: 2,
                stopped: false,
                mode: Some(ModeView { status: "Asks first".into() }),
                spend: Some(SpendView {
                    spent_micro_usd: 0,
                    limit_micro_usd: Some(5_000_000),
                    text: "$0.00 of $5.00 today".into(),
                }),
                quiet: Some(QuietView { active: false, text: "Quiet from 22:00".into() }),
            }
        );
        let bare: TrayStatus = serde_json::from_str(
            r#"{"decisions":0,"stopped":true,"stoppedText":"All agents stopped","mode":null,"spend":null}"#,
        )
        .unwrap();
        assert_eq!(bare, TrayStatus { decisions: 0, stopped: true, ..TrayStatus::default() });
    }

    #[test]
    fn the_badge_counts_up_to_nine_and_stopped_overrides_the_count() {
        let status =
            |decisions, stopped| TrayStatus { decisions, stopped, ..TrayStatus::default() };
        assert_eq!(Badge::from_status(&status(0, false)), Badge::Idle);
        assert_eq!(Badge::from_status(&status(1, false)), Badge::Count(1));
        assert_eq!(Badge::from_status(&status(9, false)), Badge::Count(9));
        assert_eq!(Badge::from_status(&status(10, false)), Badge::Many);
        assert_eq!(Badge::from_status(&status(3, true)), Badge::Stopped);
        assert_eq!(Badge::from_status(&status(0, true)), Badge::Stopped);
        assert_eq!(
            [Badge::Count(2), Badge::Many, Badge::Unreachable].map(Badge::text),
            ["2", "9+", ""]
        );
        assert_eq!(
            [Badge::Idle, Badge::Count(7), Badge::Many, Badge::SignIn].map(Badge::icon),
            ["idle", "7", "9plus", "signin"]
        );
        assert_eq!(Badge::Count(2).tooltip(), "ADK-Rust · 2 decisions wait for you");
    }
}
