//! Standing approvals: rules, their stores, the matcher, and interactive prompts
//! (spec 3.1 §7.5).

pub mod matcher;
mod prompt;
mod rule;
mod store;

pub(crate) use prompt::rule_audit_fields;
pub use prompt::{
    ApprovalPrompt, ApprovalPrompter, DEFAULT_PROMPTS_PER_TURN, PromptAnswer,
    PromptingConfirmationHandler, RENDERED_CALL_MAX_BYTES, RenderedCall, RuleHit, escape_control,
};
pub use rule::{
    ArgSpec, OriginPin, RuleOwner, RulePredicate, RuleScope, RuleSuggestion,
    SHELL_PREFIX_SUGGESTION_DENYLIST, StandingDecision, StandingRule, SuggestedScope,
};
#[cfg(feature = "postgres")]
pub use store::PostgresStandingRuleStore;
#[cfg(feature = "redis")]
pub use store::RedisStandingRuleStore;
#[cfg(feature = "sqlite")]
pub use store::SqliteStandingRuleStore;
pub use store::{FileStandingRuleStore, InMemoryStandingRuleStore, StandingRuleStore};
