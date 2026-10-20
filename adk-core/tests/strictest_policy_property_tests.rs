//! Property tests for `StrictestPolicy` (spec 3.1 §10, C2).
//!
//! - Property 1: the composed decision is exactly as strict as the strictest layer, in any
//!   layer order.
//! - Property 2: an `Allow` layer never loosens a composition, and a `Deny` layer always
//!   denies.
//! - Property 3: adding a layer the composition already names, guarded by `contains_ref`,
//!   leaves the decision and the reference unchanged.

use std::sync::Arc;

use adk_core::{
    PolicyCall, PolicyDecision, StrictestPolicy, ToolPolicy, ToolPolicyRequest, async_trait,
};
use proptest::prelude::*;
use serde_json::json;

#[derive(Debug)]
struct Fixed {
    name: String,
    decision: PolicyDecision,
}

#[async_trait]
impl ToolPolicy for Fixed {
    async fn evaluate(&self, _request: &ToolPolicyRequest) -> PolicyDecision {
        self.decision.clone()
    }

    fn policy_ref(&self) -> Option<String> {
        Some(self.name.clone())
    }
}

fn severity(decision: &PolicyDecision) -> u8 {
    match decision {
        PolicyDecision::Allow => 0,
        PolicyDecision::RequireApproval { .. } => 1,
        PolicyDecision::Deny { .. } => 2,
    }
}

fn arb_decision() -> impl Strategy<Value = PolicyDecision> {
    prop_oneof![
        Just(PolicyDecision::Allow),
        "[a-z]{1,8}".prop_map(PolicyDecision::require_approval),
        "[a-z]{1,8}".prop_map(PolicyDecision::deny),
    ]
}

fn layers(decisions: &[PolicyDecision]) -> Vec<Arc<dyn ToolPolicy>> {
    decisions
        .iter()
        .enumerate()
        .map(|(index, decision)| {
            Arc::new(Fixed { name: format!("layer-{index}"), decision: decision.clone() })
                as Arc<dyn ToolPolicy>
        })
        .collect()
}

fn decide(policy: &dyn ToolPolicy) -> PolicyDecision {
    let request = ToolPolicyRequest::new("tool", json!({}));
    futures::executor::block_on(policy.evaluate_call(&request, &PolicyCall::new()))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Property 1: the composition is as strict as its strictest layer, in any order.
    #[test]
    fn the_composition_is_as_strict_as_the_strictest_layer(
        decisions in prop::collection::vec(arb_decision(), 0..8),
        rotation in 0usize..8,
    ) {
        let strictest = decisions.iter().map(severity).max().unwrap_or(0);
        let decided = decide(&StrictestPolicy::new(layers(&decisions)));
        prop_assert_eq!(severity(&decided), strictest);

        let mut rotated = decisions.clone();
        if !rotated.is_empty() {
            let by = rotation % rotated.len();
            rotated.rotate_left(by);
        }
        let reordered = decide(&StrictestPolicy::new(layers(&rotated)));
        prop_assert_eq!(severity(&reordered), strictest);

        // The decision is one a layer gave: the first among the strictest.
        if let Some(first) = decisions.iter().find(|decision| severity(decision) == strictest) {
            prop_assert_eq!(&decided, first);
        } else {
            prop_assert_eq!(decided, PolicyDecision::Allow);
        }
    }

    /// Property 2: an `Allow` layer never loosens a composition; a `Deny` layer always denies.
    #[test]
    fn allow_never_loosens_and_deny_always_denies(
        decisions in prop::collection::vec(arb_decision(), 0..8),
        reason in "[a-z]{1,8}",
    ) {
        let base = decide(&StrictestPolicy::new(layers(&decisions)));
        let mut with_allow = decisions.clone();
        with_allow.push(PolicyDecision::Allow);
        prop_assert_eq!(decide(&StrictestPolicy::new(layers(&with_allow))), base);

        let mut with_deny = decisions.clone();
        with_deny.push(PolicyDecision::deny(reason));
        let denied = decide(&StrictestPolicy::new(layers(&with_deny)));
        prop_assert_eq!(severity(&denied), 2, "{:?}", denied);
    }

    /// Property 3: a layer added only when `contains_ref` does not already name it is added
    /// once, however many times the guard runs.
    #[test]
    fn guarded_layering_is_idempotent(
        decisions in prop::collection::vec(arb_decision(), 0..6),
        harness in arb_decision(),
        repeats in 1usize..5,
    ) {
        let layer: Arc<dyn ToolPolicy> =
            Arc::new(Fixed { name: "adk-harness".into(), decision: harness });
        let mut policy: Arc<dyn ToolPolicy> = Arc::new(StrictestPolicy::new(layers(&decisions)));
        let mut after_first = None;
        for _ in 0..repeats {
            let composed = StrictestPolicy::new(vec![policy.clone()]);
            if !composed.contains_ref("adk-harness") {
                policy = Arc::new(composed.layer(layer.clone()));
            }
            let observed = (policy.policy_ref(), decide(policy.as_ref()));
            match &after_first {
                None => after_first = Some(observed),
                Some(first) => prop_assert_eq!(first, &observed),
            }
        }
        let names = policy.policy_ref().unwrap_or_default();
        prop_assert_eq!(names.split('+').filter(|name| *name == "adk-harness").count(), 1);
    }
}
