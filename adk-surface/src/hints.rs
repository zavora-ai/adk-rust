//! Card hints (spec 3.3 §7.1, §7.2): what a feature knows about the decisions its own tools
//! ask, which a card cannot read from the call alone.
//!
//! A feature that publishes tools under its own [`ToolOrigin`](adk_core::ToolOrigin) — a
//! workflow's governed nodes, a form an agent drew, a change on the person's screens — tells
//! its decisions apart by [`ApprovalRequest::origin`], never by the tool's name, and gives
//! the engine a [`CardHint`] for each. Hints are registered with
//! [`SurfaceEngineBuilder::card_hints`](crate::SurfaceEngineBuilder::card_hints) in order, and
//! the engine reads them wherever it draws a card: Decisions, a session's decisions, the
//! feed, and the cards of the phone, voice, chat and the tray.
//!
//! | Field | What it changes | When several hints set it |
//! |---|---|---|
//! | `heading` | The heading, in place of the one made from the tool and its main argument | The first hint that sets it |
//! | `chat_heading` | The heading of a chat card | The first hint that sets it |
//! | `money` | `Some(true)` marks the call as spending money: the high-risk column, never **Always allow** (§7.4 rule 4) | Any hint that marks it |
//! | `reversibility` | "Can't be undone", "You can change it back", … | The first hint that sets it |
//! | `offer_always` | `Some(false)` removes **Always allow** | Any hint that removes it |
//! | `first_step` | A step of "How it got here" before "Asked at 10:02" | The first hint that sets it |
//! | `detail` | A line under the details, with an optional link into the home | The first hint that sets it |
//!
//! A hint may only make a card stricter or add words. It never adds an answer, never lowers
//! the risk and never changes it: the risk stays the inbox entry's, set when the call was
//! held, so chat, voice and the kernel's own routes agree (spec 3.3 §7.2). The kernel's own
//! routes, its chat gateway among them, read no hints: a feature whose hint marks money also
//! rates the call high through its approval router, as the workflow observer does. `money: Some(false)`
//! and `offer_always: Some(true)` change nothing. The engine's
//! [`heading`](crate::SurfaceEngineBuilder::heading), [`money`](crate::SurfaceEngineBuilder::money)
//! and [`reversibility`](crate::SurfaceEngineBuilder::reversibility) stay the fallback for a
//! field no hint sets.
//!
//! A hint reads only the inbox entry it is given and the provider's own kernel-side data,
//! never text an agent wrote (§7.1: every surface draws a decision from the inbox entry, never
//! from agent output).
//!
//! A feature can also name what its call acts on, as the person knows it ([`CardHints::names`]):
//! the record of an answer then carries that name and the card's summary, so "What your agents
//! did" says "Finance asked to snooze News digest; you said no" whichever answer was given
//! (spec 3.3 §8, §11).

use std::sync::Arc;

use adk_core::ApprovalRequest;
use async_trait::async_trait;

use crate::model::{CardDetail, Reversibility};

/// What a feature says about one of its decisions (spec 3.3 §7.1). Every field is optional.
/// When several hints answer the same decision, the first that sets a wording field wins, and
/// any of them can mark money or remove **Always allow**.
///
/// # Example
///
/// ```rust
/// use adk_surface::{CardDetail, CardHint, Reversibility};
///
/// let hint = CardHint::new()
///     .with_heading("Move News digest to 19:00")
///     .with_chat_heading("Snooze something in Coming up")
///     .with_reversibility(Reversibility::CanBeChangedBack)
///     .without_always_allow()
///     .with_first_step("Finance read 1 web page (example.com)");
/// assert_eq!(hint.offer_always, Some(false));
///
/// let workflow = CardHint::new()
///     .spends_money()
///     .with_detail(CardDetail::new("From the invoice-run workflow").with_link("Open in Runs", "/ui/runs/r-1"));
/// assert_eq!(workflow.money, Some(true));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardHint {
    /// What will happen, in plain words, in place of the heading made from the call.
    pub heading: Option<String>,
    /// The heading of a chat card, which carries no arguments (S-I5): "Snooze something in
    /// Coming up".
    pub chat_heading: Option<String>,
    /// `Some(true)` marks the call as spending money. `Some(false)` changes nothing.
    pub money: Option<bool>,
    /// Whether the call can be taken back.
    pub reversibility: Option<Reversibility>,
    /// `Some(false)` removes **Always allow**. `Some(true)` changes nothing: whether it is
    /// offered stays the source's and the rules' (§7.2).
    pub offer_always: Option<bool>,
    /// A step of "How it got here" before "Asked at 10:02", such as "Finance read 1 web page
    /// (example.com)".
    pub first_step: Option<String>,
    /// A line under the details, such as "From the invoice-run workflow · Open in Runs". Never
    /// on a chat card (S-I5).
    pub detail: Option<CardDetail>,
}

impl CardHint {
    /// A hint that changes nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Heads the card with `heading`.
    #[must_use]
    pub fn with_heading(mut self, heading: impl Into<String>) -> Self {
        self.heading = Some(heading.into());
        self
    }

    /// Heads a chat card with `heading`.
    #[must_use]
    pub fn with_chat_heading(mut self, heading: impl Into<String>) -> Self {
        self.chat_heading = Some(heading.into());
        self
    }

    /// Marks the call as spending money: the high-risk column, never **Always allow**.
    #[must_use]
    pub fn spends_money(mut self) -> Self {
        self.money = Some(true);
        self
    }

    /// Says whether the call can be taken back.
    #[must_use]
    pub fn with_reversibility(mut self, reversibility: Reversibility) -> Self {
        self.reversibility = Some(reversibility);
        self
    }

    /// Removes **Always allow** from the card on every surface.
    #[must_use]
    pub fn without_always_allow(mut self) -> Self {
        self.offer_always = Some(false);
        self
    }

    /// Adds `step` to "How it got here", before "Asked at 10:02".
    #[must_use]
    pub fn with_first_step(mut self, step: impl Into<String>) -> Self {
        self.first_step = Some(step.into());
        self
    }

    /// Adds the line `detail` under the details.
    #[must_use]
    pub fn with_detail(mut self, detail: CardDetail) -> Self {
        self.detail = Some(detail);
        self
    }

    /// This hint with `later`'s fields where this one sets none, keeping the stricter value of
    /// `money` and `offer_always`.
    pub(crate) fn then(self, later: Self) -> Self {
        let money = (self.marks_money() || later.marks_money()).then_some(true);
        let offer_always = (self.removes_always() || later.removes_always()).then_some(false);
        Self {
            heading: self.heading.or(later.heading),
            chat_heading: self.chat_heading.or(later.chat_heading),
            money,
            reversibility: self.reversibility.or(later.reversibility),
            offer_always,
            first_step: self.first_step.or(later.first_step),
            detail: self.detail.or(later.detail),
        }
    }

    /// The call spends money, as this hint says.
    pub(crate) fn marks_money(&self) -> bool {
        self.money == Some(true)
    }

    /// **Always allow** is not offered, as this hint says.
    pub(crate) fn removes_always(&self) -> bool {
        self.offer_always == Some(false)
    }
}

/// A feature's hints for the decisions its tools ask (spec 3.3 §7.1), read by the engine
/// wherever it draws a card.
///
/// Return `None` for every request that is not the feature's own, told by
/// [`ApprovalRequest::origin`]. Read only `request` and the feature's own kernel-side data,
/// never text an agent wrote; a failure to read that data is logged and answered with `None`
/// or a hint that says less, since the card is shown either way.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::ApprovalRequest;
/// use adk_surface::{CardHint, CardHints, LocalSource, Reversibility, SurfaceEngine};
///
/// /// The cards of tools published as `verbs:adk-rust`.
/// struct Verbs;
///
/// #[async_trait::async_trait]
/// impl CardHints for Verbs {
///     async fn hint(&self, request: &ApprovalRequest) -> Option<CardHint> {
///         let origin = request.origin.as_ref()?;
///         (origin.source == "verbs:adk-rust").then(|| {
///             CardHint::new()
///                 .with_chat_heading("Snooze something in Coming up")
///                 .with_reversibility(Reversibility::CanBeChangedBack)
///                 .without_always_allow()
///         })
///     }
/// }
///
/// # fn example(source: LocalSource) {
/// let engine = SurfaceEngine::builder(Arc::new(source)).card_hints(Arc::new(Verbs)).build();
/// # let _ = engine;
/// # }
/// ```
#[async_trait]
pub trait CardHints: Send + Sync {
    /// The hint for `request`, or `None` when it is not this feature's.
    async fn hint(&self, request: &ApprovalRequest) -> Option<CardHint>;

    /// What `request`'s call acts on, as the person knows it — "News digest" — which the
    /// record of an answer to it names beside the card's summary (spec 3.3 §8, §11). `None`,
    /// the default, for a call that names nothing a person knows or that is not this
    /// feature's.
    async fn names(&self, _request: &ApprovalRequest) -> Option<String> {
        None
    }
}

/// What `request`'s call acts on, as the first of `hints` that names it says, or for a decision
/// the kernel itself asks, what it is about: the agent to add, the agenda item, the driver
/// (spec 3.3 §8).
pub(crate) async fn named(
    hints: &[Arc<dyn CardHints>],
    request: &ApprovalRequest,
) -> Option<String> {
    for hints in hints {
        if let Some(name) = hints.names(request).await {
            return Some(name);
        }
    }
    crate::project::kernel_name(request)
}

#[cfg(test)]
mod tests {
    use adk_core::{
        ApprovalPolicy, ApprovalRisk, ApprovalScope, ApproverSelector, ToolConfirmationRequest,
    };
    use chrono::FixedOffset;
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;
    use crate::origin::{Caller, Origin, SurfaceKind};
    use crate::project::Projector;
    use crate::rules::{Answer, allowed_answers};

    fn words() -> impl Strategy<Value = Option<String>> {
        proptest::option::of("[A-Za-z ]{1,12}")
    }

    fn reversibility() -> impl Strategy<Value = Option<Reversibility>> {
        proptest::option::of(prop_oneof![
            Just(Reversibility::CantBeUnsent),
            Just(Reversibility::CantBeUndone),
            Just(Reversibility::CanBeRemoved),
            Just(Reversibility::CanBeChangedBack),
        ])
    }

    fn hint() -> impl Strategy<Value = CardHint> {
        (
            (words(), words(), any::<Option<bool>>()),
            (reversibility(), any::<Option<bool>>(), words(), words()),
        )
            .prop_map(
                |((heading, chat_heading, money), (reversibility, offer_always, step, detail))| {
                    CardHint {
                        heading,
                        chat_heading,
                        money,
                        reversibility,
                        offer_always,
                        first_step: step,
                        detail: detail.map(CardDetail::new),
                    }
                },
            )
    }

    fn caller(row: usize) -> Caller {
        let (surface, origin) = match row {
            0 => (SurfaceKind::Web, Origin::Pointer),
            1 => (SurfaceKind::Terminal, Origin::Keyboard),
            2 => (SurfaceKind::Phone, Origin::Touch),
            3 => (SurfaceKind::Tray, Origin::Pointer),
            4 => (SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() }),
            _ => (SurfaceKind::Web, Origin::Voice),
        };
        let caller = Caller::human("ana", surface, origin);
        if row == 1 { caller.with_paired_local() } else { caller }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Spec 3.3 §7.1: the first hint that sets a field wins; money and Always allow keep
        /// the stricter value any hint gives.
        #[test]
        fn hints_merge_first_wins_and_stricter_counts(hints in proptest::collection::vec(hint(), 0..5)) {
            let merged = hints.iter().cloned().fold(CardHint::new(), CardHint::then);
            let first = |field: fn(&CardHint) -> Option<String>| hints.iter().find_map(field);
            prop_assert_eq!(merged.heading.clone(), first(|hint| hint.heading.clone()));
            prop_assert_eq!(merged.chat_heading.clone(), first(|hint| hint.chat_heading.clone()));
            prop_assert_eq!(merged.first_step.clone(), first(|hint| hint.first_step.clone()));
            prop_assert_eq!(
                merged.detail.clone(),
                hints.iter().find_map(|hint| hint.detail.clone())
            );
            prop_assert_eq!(merged.reversibility, hints.iter().find_map(|hint| hint.reversibility));
            prop_assert_eq!(merged.marks_money(), hints.iter().any(CardHint::marks_money));
            prop_assert_eq!(merged.removes_always(), hints.iter().any(CardHint::removes_always));
        }

        /// Spec 3.3 §7.2: whatever a hint says, every surface may give no answer it could not
        /// give without it, the risk is the inbox entry's, and money is never unmarked.
        #[test]
        fn a_hint_never_adds_an_answer_or_changes_the_risk(
            hints in proptest::collection::vec(hint(), 0..4),
            risk in prop_oneof![
                Just(ApprovalRisk::Low),
                Just(ApprovalRisk::Medium),
                Just(ApprovalRisk::High),
                Just(ApprovalRisk::Critical),
            ],
            tool in prop_oneof![Just("payments_charge"), Just("send_email"), Just("verb_snooze")],
            always in any::<bool>(),
            row in 0usize..6,
        ) {
            let merged = hints.into_iter().fold(CardHint::new(), CardHint::then);
            let projector = Projector::new(FixedOffset::east_opt(0).unwrap());
            let call = ToolConfirmationRequest::new(tool, json!({ "to": "[client email]" }));
            let request = adk_core::ApprovalRequest::new(
                "apr-1",
                ApprovalScope::new("adk-rust", "ana", "s1"),
                &call,
                ApprovalPolicy::new([ApproverSelector::human("ana")]),
            )
            .with_risk(risk);
            let label = always.then(|| "Always allow for this person".to_string());
            let plain = projector.facts(&request, label.clone(), &CardHint::new());
            let hinted = projector.facts(&request, label, &merged);
            prop_assert_eq!(hinted.risk, plain.risk);
            prop_assert_eq!(&hinted.allows, &plain.allows);
            prop_assert!(hinted.money || !plain.money);
            let caller = caller(row);
            let answers = |facts| -> Vec<Answer> {
                allowed_answers(&caller, facts).options.iter().map(|option| option.answer).collect()
            };
            let (before, after) = (answers(&plain), answers(&hinted));
            prop_assert!(after.iter().all(|answer| before.contains(answer)), "{before:?} → {after:?}");
        }
    }
}
