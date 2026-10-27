//! Properties of screen actions (spec 3.3 §2 item 7, §5.6; owner question 22): an agent's
//! change always waits for a person, snooze changes only when an item wakes, a spoken action
//! needs one plain title, and an action's declaration digest never drifts.

#![cfg(all(feature = "verb-ui", feature = "web"))]

mod verbs_support;

use std::sync::Arc;

use adk_core::principal::SubjectKind;
use adk_core::{
    ActingContext, InMemoryApprovalStore, PolicyCall, PolicyDecision, StoreApprovalService,
    ToolOrigin, ToolPolicy, ToolPolicyRequest,
};
use adk_kernel::{AgendaState, PartitionId, Wake, WakeSet};
use adk_session::InMemorySessionService;
use adk_surface::verbs::spoken::{SpokenAction, parse};
use adk_surface::verbs::{ComingUpItem, ObjectRef, VerbGate, VerbIntent};
use adk_surface::{LocalSource, SurfaceEngine};
use chrono::{DateTime, Duration as Span, Utc};
use proptest::prelude::*;
use serde_json::json;
use verbs_support::*;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

/// An engine over no kernel: the gate records through it.
fn engine() -> SurfaceEngine {
    let source = LocalSource::builder(APP)
        .sessions(Arc::new(InMemorySessionService::new()))
        .approvals(Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))))
        .build()
        .unwrap();
    SurfaceEngine::builder(Arc::new(source)).build()
}

fn item(id: &str, title: &str) -> ComingUpItem {
    ComingUpItem {
        id: id.into(),
        title: title.into(),
        area: "default".into(),
        at: None,
        at_text: String::new(),
        ends: None,
        choices: vec![],
        why_not: None,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Owner question 22: every changing action from any agent principal waits for a person,
    /// with the kernel's words.
    #[test]
    fn a_change_from_any_agent_waits_for_a_person(
        agent in "[a-z][a-z0-9_]{0,20}",
        tomorrow in any::<bool>(),
        hours in 2i64..200,
    ) {
        let rt = runtime();
        let agenda = MemoryAgenda::default();
        let at = digest_at() + Span::hours(hours);
        agenda.put(scheduled(DIGEST, "News digest", OWNER, "default", at));
        let registry = memory_registry(&agenda).build().unwrap();
        let to = if tomorrow { at + Span::days(1) } else { at + Span::hours(1) };
        let mut request = ToolPolicyRequest::new(
            "verb_snooze",
            json!({ "item": DIGEST, "to": to.with_timezone(&offset()).to_rfc3339() }),
        );
        request.user_id = OWNER.into();
        request.agent_name = agent.clone();
        request.acting = Some(ActingContext::derived(APP, &agent, OWNER, SubjectKind::Human));
        let origin = ToolOrigin::new(registry.origin(), &json!({}), None);
        let mut call = PolicyCall::new();
        call.origin = Some(&origin);
        let decision = rt.block_on(registry.policy(PartitionId::default()).evaluate_call(&request, &call));
        prop_assert_eq!(
            decision,
            PolicyDecision::RequireApproval { reason: format!("Move News digest to {}", time_text(to)) }
        );
    }

    /// Spec 3.3 §2 item 7: snooze changes only when an item wakes, never what state it is in;
    /// a refused snooze changes nothing.
    #[test]
    fn snooze_changes_only_the_wake(
        state in 0usize..5,
        by in prop_oneof![Just(Span::hours(1)), Just(Span::days(1)), Just(Span::hours(2)), Just(Span::hours(-1))],
        started in any::<bool>(),
        ends in proptest::option::of(0i64..48),
    ) {
        let rt = runtime();
        let agenda = MemoryAgenda::default();
        let at = digest_at();
        let mut before = scheduled(DIGEST, "News digest", OWNER, "default", at);
        before.state = match state {
            0 | 1 => AgendaState::Scheduled { wake: WakeSet::any_of([Wake::At(at)]) },
            2 => AgendaState::Scheduled { wake: WakeSet::any_of([Wake::Manual]) },
            3 => AgendaState::Running { pid: "run-1".into() },
            _ => AgendaState::Approved,
        };
        before.expires_at = ends.map(|hours| at + Span::hours(hours));
        agenda.put(before.clone());
        if started {
            agenda.start(DIGEST);
        }
        let gate = VerbGate::new(memory_registry(&agenda).build().unwrap(), engine());
        let to: DateTime<Utc> = at + by;
        let intent = VerbIntent::new("snooze", json!({ "to": to.with_timezone(&offset()).to_rfc3339() }))
            .on(ObjectRef::coming_up(DIGEST));
        let result = rt.block_on(gate.call(&web(), intent));
        let after = agenda.get(DIGEST).unwrap();
        prop_assert_eq!(after.state.kind(), before.state.kind());
        match result {
            Ok(_) => {
                let mut expected = before.clone();
                expected.state = AgendaState::Scheduled { wake: WakeSet::any_of([Wake::At(to)]) };
                prop_assert_eq!(after, expected);
                prop_assert!(state < 2 && !started && (by == Span::hours(1) || by == Span::days(1)));
                prop_assert!(before.expires_at.is_none_or(|ends| to <= ends));
            }
            Err(_) => prop_assert_eq!(after, before),
        }
    }

    /// Spec 3.3 §5.6: a spoken action never says "always", has at most eight words, and names
    /// exactly one title; words that are part of several titles are ambiguous.
    #[test]
    fn a_spoken_action_needs_one_plain_title(
        words in proptest::collection::vec("[a-z]{3,7}", 1..4),
        extra in 0usize..10,
        always in any::<bool>(),
    ) {
        prop_assume!(words.iter().all(|word| !["please", "always", "snooze", "tomorrow", "until", "weekly"].contains(&word.as_str())));
        let title = words.join(" ");
        let items = [item("a", &title), item("b", &format!("{title} weekly")), item("c", "Water plants")];
        // The exact title: one item, unless the utterance is too long or says "always".
        let mut said: Vec<String> = vec!["snooze".into()];
        said.extend(words.iter().cloned());
        said.extend((0..extra).map(|_| "please".to_string()));
        if always {
            said.insert(1, "always".into());
        }
        let parsed = parse(&said.join(" "), &items);
        if always || said.len() > adk_surface::verbs::spoken::MAX_WORDS {
            prop_assert_eq!(parsed, None);
        } else {
            prop_assert_eq!(
                parsed,
                Some(SpokenAction::Snooze { item: "a".into(), title: title.clone(), tomorrow: false })
            );
        }
        // Words in two titles, and no title at all, run nothing.
        let partial = parse(&format!("snooze {}", words[0]), &items);
        if words.len() > 1 {
            prop_assert!(matches!(partial, Some(SpokenAction::Ambiguous { .. })), "{partial:?}");
        }
        prop_assert_eq!(parse("snooze", &items), None);
        prop_assert_eq!(parse(&format!("snooze {} qqqqqqqqq", words[0]), &items), None);
    }

    /// The digest of an action's declaration is the same however often the registry is built,
    /// so a standing decision or a held call keeps matching it.
    #[test]
    fn the_declaration_digest_is_stable(app in "[a-z][a-z-]{0,12}") {
        let origins = |app: &str| -> Vec<ToolOrigin> {
            let agenda = MemoryAgenda::default();
            let registry = adk_surface::verbs::VerbRegistry::builder(app, Arc::new(agenda)).build().unwrap();
            registry
                .toolset(PartitionId::default())
                .tools()
                .iter()
                .map(|tool| ToolOrigin::of(tool.as_ref()))
                .collect()
        };
        let first = origins(&app);
        prop_assert_eq!(&first, &origins(&app));
        prop_assert_eq!(first.len(), 2);
        let source = format!("verbs:{app}");
        prop_assert!(first.iter().all(|origin| origin.source == source));
        let other: Vec<String> = origins("adk-rust").into_iter().map(|origin| origin.declaration_digest).collect();
        let mine: Vec<String> = first.into_iter().map(|origin| origin.declaration_digest).collect();
        prop_assert_eq!(mine, other, "the declaration does not name the app");
    }
}
