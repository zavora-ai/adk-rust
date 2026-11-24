//! Standing approvals: rules, their stores, the matcher, and interactive prompts
//! (spec 3.1 §7.5).

mod batch;
mod changeset;
#[cfg(all(feature = "computer-use", feature = "runner"))]
mod held_plan;
pub mod matcher;
mod prompt;
mod rule;
#[cfg(feature = "computer-use")]
mod screen_plan;
mod store;

pub use changeset::{
    CHANGE_SET_MAX_BYTES, ChangeSet, ChangeSetAnswer, ChangeSetCall, ChangeSetFile,
    ChangeSetPrompt, RenderedChangeSet,
};
#[cfg(all(feature = "computer-use", feature = "runner"))]
pub use held_plan::{HeldPlanState, HeldScreenPlan};
pub(crate) use prompt::rule_audit_fields;
pub use prompt::{
    ApprovalPrompt, ApprovalPrompter, DEFAULT_PROMPTS_PER_TURN, PromptAnswer,
    PromptingConfirmationHandler, RENDERED_CALL_MAX_BYTES, RenderedCall, RuleHit, escape_control,
};
pub use rule::{
    ArgSpec, OriginPin, RuleOwner, RulePredicate, RuleScope, RuleSuggestion,
    SHELL_PREFIX_SUGGESTION_DENYLIST, StandingDecision, StandingRule, SuggestedScope,
};
#[cfg(feature = "computer-use")]
pub use screen_plan::{
    RenderedScreenPlan, SCREEN_PLAN_MAX_BYTES, ScreenPlanAnswer, ScreenPlanPrompt,
};
#[cfg(feature = "postgres")]
pub use store::PostgresStandingRuleStore;
#[cfg(feature = "redis")]
pub use store::RedisStandingRuleStore;
#[cfg(feature = "sqlite")]
pub use store::SqliteStandingRuleStore;
pub use store::{FileStandingRuleStore, InMemoryStandingRuleStore, StandingRuleStore};
