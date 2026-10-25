//! Properties of voice (spec 3.3 §5.6, §7.4 rule 4): what words can answer, and that the
//! camera and the screen reach the model only while their switches are on.

#![cfg(feature = "voice")]

use std::time::Duration;

use adk_surface::voice::spoken::{MAX_WORDS, Spoken, parse};
use adk_surface::voice::{FramePolicy, VisualInputs, VisualSource};
use proptest::prelude::*;
use tokio::time::Instant;

/// Words a person might say around an answer.
fn word() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "allow", "once", "deny", "it", "please", "always", "yes", "no", "sure", "ok", "cancel",
        "stop", "all", "send", "the", "invoice", "say", "or", "go", "ahead", "approve",
    ])
    .prop_map(str::to_string)
}

/// Punctuation and spacing between words.
fn gap() -> impl Strategy<Value = String> {
    prop::sample::select(vec![" ", ", ", ". ", "! ", "  ", " - ", "? "]).prop_map(str::to_string)
}

fn utterance() -> impl Strategy<Value = Vec<(String, String)>> {
    prop::collection::vec((word(), gap()), 1..8)
}

fn join(parts: &[(String, String)]) -> String {
    parts.iter().map(|(word, gap)| format!("{word}{gap}")).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// "always" never allows anything.
    #[test]
    fn always_never_allows(parts in utterance(), pending: bool) {
        let mut parts = parts;
        parts.push(("always".into(), " ".into()));
        let said = join(&parts);
        prop_assert_ne!(parse(&said, pending), Some(Spoken::AllowOnce), "{}", said);
        prop_assert_ne!(parse(&said, pending), Some(Spoken::Deny), "{}", said);
    }

    /// An utterance with both an allow and a deny decides nothing.
    #[test]
    fn allow_and_deny_together_decide_nothing(before in utterance(), after in utterance(), pending: bool) {
        let said = format!("{} allow once {} deny {}", join(&before), join(&after), join(&before));
        prop_assert!(!matches!(parse(&said, pending), Some(Spoken::AllowOnce | Spoken::Deny)), "{}", said);
    }

    /// More than four words decide nothing (only "stop all" stops whatever surrounds it).
    #[test]
    fn long_utterances_decide_nothing(parts in prop::collection::vec((word(), gap()), (MAX_WORDS + 1)..12), pending: bool) {
        let said = join(&parts);
        prop_assert!(!matches!(parse(&said, pending), Some(Spoken::AllowOnce | Spoken::Deny | Spoken::Cancel)), "{}", said);
    }

    /// Case and punctuation never change what an utterance means.
    #[test]
    fn case_and_punctuation_never_matter(parts in utterance(), upper in prop::collection::vec(any::<bool>(), 8), pending: bool) {
        let plain: String = parts.iter().map(|(word, _)| word.as_str()).collect::<Vec<_>>().join(" ");
        let styled: String = parts
            .iter()
            .zip(upper.iter().cycle())
            .map(|((word, gap), upper)| {
                let word = if *upper { word.to_uppercase() } else { word.clone() };
                format!("{word}{gap}")
            })
            .collect();
        prop_assert_eq!(parse(&plain, pending), parse(&styled, pending), "{} / {}", plain, styled);
    }

    /// "cancel" means something only while a spoken answer is pending.
    #[test]
    fn cancel_needs_a_pending_answer(parts in utterance()) {
        let said = join(&parts);
        prop_assert_ne!(parse(&said, false), Some(Spoken::Cancel), "{}", said);
    }
}

/// One step of what reaches the gate.
#[derive(Debug, Clone)]
enum Step {
    Switch(VisualSource, bool),
    Frame(VisualSource, u8, u64),
}

fn source() -> impl Strategy<Value = VisualSource> {
    prop_oneof![Just(VisualSource::Camera), Just(VisualSource::Screen)]
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            (source(), any::<bool>()).prop_map(|(source, on)| Step::Switch(source, on)),
            (source(), 0u8..4, 0u64..3_000)
                .prop_map(|(source, frame, after)| Step::Frame(source, frame, after)),
        ],
        1..60,
    )
}

fn policy() -> impl Strategy<Value = FramePolicy> {
    prop_oneof![Just(FramePolicy::GEMINI), Just(FramePolicy::OPENAI)]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The forwarded frames are exactly those that arrived while their source's switch was
    /// on, no closer together than the policy allows, and none arrives after an off until the
    /// next on.
    #[test]
    fn frames_flow_only_while_their_switch_is_on(policy in policy(), steps in steps()) {
        let interval = match policy {
            FramePolicy::Steady { interval } | FramePolicy::OnChange { interval } => interval,
        };
        let mut gate = VisualInputs::new(policy);
        let start = Instant::now();
        let mut now = start;
        let mut on = [false, false];
        let mut last: [Option<(Instant, u8)>; 2] = [None, None];
        let index = |source: VisualSource| match source {
            VisualSource::Camera => 0,
            VisualSource::Screen => 1,
        };
        for step in steps {
            match step {
                Step::Switch(source, switch) => {
                    gate.switch(source, switch);
                    on[index(source)] = switch;
                    if !switch {
                        last[index(source)] = last[index(source)].map(|(at, _)| (at, u8::MAX));
                    }
                }
                Step::Frame(source, frame, after) => {
                    now += Duration::from_millis(after);
                    let forwarded = gate.offer(source, &[frame], now);
                    let i = index(source);
                    if !on[i] {
                        prop_assert!(!forwarded, "a frame of a source that is off went through");
                        continue;
                    }
                    let spaced = last[i].is_none_or(|(at, _)| now.duration_since(at) >= interval);
                    let changed = match policy {
                        FramePolicy::OnChange { .. } => last[i].is_none_or(|(_, previous)| previous != frame),
                        FramePolicy::Steady { .. } => true,
                    };
                    prop_assert_eq!(forwarded, spaced && changed);
                    if forwarded {
                        last[i] = Some((now, frame));
                    }
                }
            }
        }
    }
}
