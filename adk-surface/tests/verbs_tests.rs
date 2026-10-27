//! Screen actions (spec 3.3 §2 item 7, §7.1, §7.2, §11; owner questions 21–27): the catalogue
//! and its names, what a handler can reach, the policy's decision table, snooze's refusals, the
//! areas an agent sees, an agent's held request and its card on every surface, Stop all, the
//! web routes, the kernel's records route, the terminal and the chat heading.

#![cfg(all(feature = "verb-ui", feature = "web"))]

#[cfg(feature = "channels")]
mod fake_telegram;
mod verbs_support;

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::AuditDecision;
use adk_core::principal::SubjectKind;
use adk_core::{
    ActingContext, ApprovalAction, ApprovalPolicy, ApprovalRequest, ApprovalRequestQuery,
    ApprovalRisk, ApprovalScope, ApprovalState, ApproverSelector, InMemoryApprovalStore,
    PolicyCall, PolicyDecision, RequestContext, Tool, ToolConfirmationRequest, ToolOrigin,
    ToolPolicy, ToolPolicyRequest,
};
use adk_kernel::drivers::EffectClass;
use adk_kernel::{AgendaState, PartitionId, Wake, WakeSet};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_surface::verbs::{
    Callers, ObjectRef, VerbDone, VerbError, VerbHandler, VerbIntent, VerbScope, VerbSpec,
    check_name, find_by_title,
};
use adk_surface::{Answer, Command, CommandOutcome};
use chrono::{Duration as Span, Utc};
use serde_json::{Value, json};
use verbs_support::*;

/// The intent to snooze `id` to `to`.
fn snooze(id: &str, to: chrono::DateTime<Utc>) -> VerbIntent {
    VerbIntent::new("snooze", json!({ "to": to.with_timezone(&offset()).to_rfc3339() }))
        .on(ObjectRef::coming_up(id))
}

mod catalogue {
    use super::*;

    #[test]
    fn reserved_and_malformed_names_are_refused() {
        for name in adk_surface::verbs::RESERVED {
            assert_eq!(check_name(name).unwrap_err().code(), "surface.verb.reserved", "{name}");
        }
        for name in ["", "Snooze", "1snooze", "snooze-it", "snooze it", &"a".repeat(60)] {
            assert_eq!(check_name(name).unwrap_err().code(), "surface.verb.invalid", "{name:?}");
        }
        for name in ["snooze", "coming_up", "a", &"a".repeat(59), "pin2"] {
            assert!(check_name(name).is_ok(), "{name:?}");
        }
    }

    struct Nothing;

    #[async_trait::async_trait]
    impl VerbHandler for Nothing {
        async fn describe(&self, _: &VerbScope, _: &Value) -> Result<String, VerbError> {
            Ok("Nothing".into())
        }

        async fn run(&self, _: &VerbScope, _: &Value) -> Result<VerbDone, VerbError> {
            Ok(VerbDone::new("nothing", "Nothing happened"))
        }
    }

    #[test]
    fn a_host_cannot_declare_a_reserved_or_second_action() {
        let agenda = MemoryAgenda::default();
        let declare = |name: &str| {
            memory_registry(&agenda)
                .verb(VerbSpec::new(name, "X", "X", EffectClass::WriteLocal), Arc::new(Nothing))
                .build()
                .map(|_| ())
                .unwrap_err()
                .code()
        };
        assert_eq!(declare("approve"), "surface.verb.reserved");
        assert_eq!(declare("resume_all"), "surface.verb.reserved");
        assert_eq!(declare("snooze"), "surface.verb.invalid", "the catalogue's own name");
        assert_eq!(declare("Pin"), "surface.verb.invalid");
        let registry = memory_registry(&agenda)
            .verb(
                VerbSpec::new("pin", "Pin", "Pin something", EffectClass::WriteLocal),
                Arc::new(Nothing),
            )
            .build()
            .unwrap();
        let names: Vec<String> = registry.views().into_iter().map(|view| view.name).collect();
        assert_eq!(names, ["coming_up", "snooze", "pin"]);
    }

    /// The first catalogue (owner question 21).
    #[test]
    fn the_catalogue_is_coming_up_and_snooze() {
        let registry = memory_registry(&MemoryAgenda::default()).build().unwrap();
        let views = serde_json::to_value(registry.views()).unwrap();
        assert_eq!(
            views,
            json!([
                {
                    "name": "coming_up", "label": "Coming up",
                    "description": registry.spec("coming_up").unwrap().description,
                    "objects": ["comingUp"], "effect": "read", "agentsAsk": true,
                    "onlyDelays": false, "reversible": false,
                },
                {
                    "name": "snooze", "label": "Snooze",
                    "description": "Snooze something in Coming up",
                    "objects": ["comingUp"], "effect": "write_local", "agentsAsk": true,
                    "onlyDelays": true, "reversible": true,
                },
            ])
        );
        assert_eq!(registry.origin(), "verbs:adk-rust");
        assert_eq!(
            registry.grant().text,
            "Ask to change things on your screens — always asks you first"
        );
    }

    /// An action takes only the object kinds it declares.
    #[tokio::test]
    async fn an_object_the_action_does_not_take_is_refused() {
        let agenda = MemoryAgenda::default();
        let registry = memory_registry(&agenda)
            .verb(
                VerbSpec::new("pin", "Pin", "Pin something", EffectClass::WriteLocal),
                Arc::new(Nothing),
            )
            .build()
            .unwrap();
        let home =
            home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new())))).await;
        let gate = adk_surface::verbs::VerbGate::new(registry, home.engine.clone());
        let intent = VerbIntent::new("pin", json!({})).on(ObjectRef::coming_up(DIGEST));
        assert_eq!(gate.call(&web(), intent).await.unwrap_err().code(), "surface.verb.invalid");
        let unknown = VerbIntent::new("sweep", json!({}));
        assert_eq!(gate.call(&web(), unknown).await.unwrap_err().code(), "surface.verb.unknown");
    }
}

mod handler {
    use super::*;

    /// What a handler sees: the caller's items and nothing else (S-I4, S-I10, S-I11). The
    /// scope has no approval service, rules, modes or governance to reach — the doctest on
    /// `VerbScope` fails to compile when one tries — and the inbox is untouched by any run.
    struct Probe(Arc<std::sync::Mutex<Vec<String>>>);

    #[async_trait::async_trait]
    impl VerbHandler for Probe {
        async fn describe(&self, _: &VerbScope, _: &Value) -> Result<String, VerbError> {
            Ok("Probe".into())
        }

        async fn run(&self, scope: &VerbScope, _: &Value) -> Result<VerbDone, VerbError> {
            let titles: Vec<String> =
                scope.items().await?.into_iter().map(|item| item.title).collect();
            self.0.lock().unwrap().extend(titles);
            // Another person's item is not found, as one that does not exist.
            let theirs = scope.item("bens-item").await.unwrap_err();
            let none = scope.item("no-such-item").await.unwrap_err();
            assert_eq!(theirs.to_string(), none.to_string().replace("no-such-item", "bens-item"));
            Ok(VerbDone::new("probe", "Probed"))
        }
    }

    #[tokio::test]
    async fn a_handler_reaches_only_the_callers_items_and_never_the_inbox() {
        let agenda = MemoryAgenda::default();
        let at = digest_at();
        agenda.put(scheduled("mine", "News digest", OWNER, "default", at));
        agenda.put(scheduled("bens-item", "Ben's report", "ben", "default", at));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let store = Arc::new(InMemoryApprovalStore::new());
        let registry = memory_registry(&agenda)
            .approval_store(store.clone())
            .verb(
                VerbSpec::new("probe", "Probe", "Probe", EffectClass::Read)
                    .with_callers(Callers::PersonAndAgents),
                Arc::new(Probe(seen.clone())),
            )
            .build()
            .unwrap();
        let home =
            home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new())))).await;
        let gate = adk_surface::verbs::VerbGate::new(registry, home.engine.clone());
        gate.call(&web(), VerbIntent::new("probe", json!({}))).await.unwrap();
        assert_eq!(*seen.lock().unwrap(), ["News digest"]);
        use adk_core::ApprovalStore as _;
        assert!(store.list_requests(&ApprovalRequestQuery::default()).await.unwrap().is_empty());
    }
}

mod policy {
    use super::*;

    /// The tool `name` of `registry`, as an agent in the default area gets it.
    fn tool(registry: &adk_surface::verbs::VerbRegistry, name: &str) -> Arc<dyn Tool> {
        registry
            .toolset(PartitionId::default())
            .tools()
            .into_iter()
            .find(|tool| tool.name() == name)
            .unwrap()
    }

    fn request(tool: &str, agent: &str, args: Value) -> ToolPolicyRequest {
        let mut request = ToolPolicyRequest::new(tool, args);
        request.agent_name = agent.into();
        request.app_name = APP.into();
        request.user_id = OWNER.into();
        request.acting = Some(ActingContext::derived(APP, agent, OWNER, SubjectKind::Human));
        request
    }

    #[derive(Debug, PartialEq)]
    enum Decided {
        Allow,
        Ask(String),
        Deny(String),
    }

    fn decided(decision: PolicyDecision) -> Decided {
        match decision {
            PolicyDecision::Allow => Decided::Allow,
            PolicyDecision::RequireApproval { reason } => Decided::Ask(reason),
            PolicyDecision::Deny { reason } => Decided::Deny(reason),
        }
    }

    /// Waiting requests of `agent` in `store`.
    async fn waiting(
        store: &InMemoryApprovalStore,
        agent: &str,
        count: usize,
        origin: &ToolOrigin,
    ) {
        use adk_core::ApprovalStore as _;
        for index in 0..count {
            let call =
                ToolConfirmationRequest::new("verb_snooze", json!({ "item": format!("i{index}") }))
                    .with_acting_context(ActingContext::derived(
                        APP,
                        agent,
                        OWNER,
                        SubjectKind::Human,
                    ))
                    .with_origin(origin.clone());
            let request = ApprovalRequest::new(
                format!("apr-{agent}-{index}"),
                ApprovalScope::new(APP, OWNER, "s1"),
                &call,
                ApprovalPolicy::new([ApproverSelector::human(OWNER)]),
            )
            .with_acting(ActingContext::derived(APP, agent, OWNER, SubjectKind::Human));
            store.create_request(&request).await.unwrap();
        }
    }

    struct Tidy;

    #[async_trait::async_trait]
    impl VerbHandler for Tidy {
        async fn describe(&self, _: &VerbScope, _: &Value) -> Result<String, VerbError> {
            Ok("Tidy Coming up".into())
        }

        async fn run(&self, _: &VerbScope, _: &Value) -> Result<VerbDone, VerbError> {
            Ok(VerbDone::new("tidy", "Tidied"))
        }
    }

    /// One row of the table: what, the tool's origin, the agent, the arguments, the decision.
    type Case<'a> = (&'a str, &'a ToolOrigin, &'a str, Value, Decided);

    /// Spec 3.3 §2 item 7, owner question 22: every combination of caller, effect, callers and
    /// cap.
    #[tokio::test]
    async fn the_policy_decides_every_combination() {
        let agenda = MemoryAgenda::default();
        let at = digest_at();
        agenda.put(scheduled(DIGEST, "News digest", OWNER, "default", at));
        agenda.put(scheduled(PLANTS, "Water plants", OWNER, "home", at));
        let store = Arc::new(InMemoryApprovalStore::new());
        let registry = memory_registry(&agenda)
            .approval_store(store.clone())
            .agent_name("finance", "Finance")
            .verb(
                VerbSpec::new("tidy", "Tidy", "Tidy Coming up", EffectClass::WriteLocal)
                    .with_callers(Callers::PersonOnly),
                Arc::new(Tidy),
            )
            .build()
            .unwrap();
        let policy = registry.policy(PartitionId::default());
        let snooze_origin = ToolOrigin::of(tool(&registry, "verb_snooze").as_ref());
        let read_origin = ToolOrigin::of(tool(&registry, "verb_coming_up").as_ref());
        let other = ToolOrigin::new("mcp:calendar", &json!({ "name": "verb_snooze" }), None);
        let to = |at: chrono::DateTime<Utc>| at.with_timezone(&offset()).to_rfc3339();
        let valid = json!({ "item": DIGEST, "to": to(at + Span::hours(1)) });
        let move_text = format!("Move News digest to {}", time_text(at + Span::hours(1)));
        waiting(&store, "busy", 3, &snooze_origin).await;
        waiting(&store, "finance", 2, &snooze_origin).await;
        let cases: Vec<Case<'_>> = vec![
            ("not a screen action", &other, "finance", valid.clone(), Decided::Allow),
            ("a person's own", &snooze_origin, "surface.verbs", valid.clone(), Decided::Allow),
            ("a read", &read_origin, "finance", json!({}), Decided::Allow),
            (
                "a change, every time",
                &snooze_origin,
                "finance",
                valid.clone(),
                Decided::Ask(move_text.clone()),
            ),
            (
                "three already waiting",
                &snooze_origin,
                "busy",
                valid.clone(),
                Decided::Deny("Busy already has 3 changes waiting for you".into()),
            ),
            (
                "another area's item",
                &snooze_origin,
                "finance",
                json!({ "item": PLANTS, "to": to(at + Span::hours(1)) }),
                Decided::Deny(format!("nothing in Coming up is called {PLANTS}")),
            ),
            (
                "a time it cannot move to",
                &snooze_origin,
                "finance",
                json!({ "item": DIGEST, "to": to(at + Span::hours(2)) }),
                Decided::Deny(format!(
                    "It can only move to {} or {}",
                    time_text(at + Span::hours(1)),
                    time_text(at + Span::days(1))
                )),
            ),
            (
                "a time it is already at",
                &snooze_origin,
                "finance",
                json!({ "item": DIGEST, "to": to(at) }),
                Decided::Deny("It moved since you saw this".into()),
            ),
        ];
        for (what, origin, agent, args, expected) in cases {
            let tool_name = if origin.source == read_origin.source && args == json!({}) {
                "verb_coming_up"
            } else {
                "verb_snooze"
            };
            let mut call = PolicyCall::new();
            call.origin = Some(origin);
            let decision = policy.evaluate_call(&request(tool_name, agent, args), &call).await;
            assert_eq!(decided(decision), expected, "{what}");
        }
        // An action only a person takes is refused to every agent.
        let tidy = ToolOrigin::new("verbs:adk-rust", &json!({}), None);
        let mut call = PolicyCall::new();
        call.origin = Some(&tidy);
        let decision =
            policy.evaluate_call(&request("verb_tidy", "finance", json!({})), &call).await;
        assert!(matches!(decided(decision), Decided::Deny(_)));
        // A person's own runs, whatever the action.
        let decision =
            policy.evaluate_call(&request("verb_tidy", "surface.verbs", json!({})), &call).await;
        assert_eq!(decided(decision), Decided::Allow);
    }

    /// Owner question 22: the cap counts the requests waiting for the person the agent works
    /// for, on every page of the store, and never refuses a call the person already answered —
    /// a held call resuming with its decision, or one the person edited.
    #[tokio::test]
    async fn the_cap_counts_one_persons_waiting_requests_and_spares_answered_calls() {
        use adk_core::{ApprovalDecision, ApprovalStore as _, ToolApproval};
        let agenda = MemoryAgenda::default();
        let at = digest_at();
        agenda.put(scheduled(DIGEST, "News digest", OWNER, "default", at));
        let fresh = || async {
            let store = Arc::new(InMemoryApprovalStore::new());
            let registry = memory_registry(&agenda)
                .approval_store(store.clone())
                .agent_name("finance", "Finance")
                .build()
                .unwrap();
            (store, registry)
        };
        let to = (at + Span::hours(1)).with_timezone(&offset()).to_rfc3339();
        let args = json!({ "item": DIGEST, "to": to });
        let move_text = format!("Move News digest to {}", time_text(at + Span::hours(1)));
        let request_for = |args: Value, session: &str| {
            let mut request = request("verb_snooze", "finance", args);
            request.session_id = session.into();
            request
        };
        let pending = |id: String, user: &str, origin: &ToolOrigin| {
            let call = ToolConfirmationRequest::new("verb_snooze", json!({ "item": id.clone() }))
                .with_acting_context(ActingContext::derived(
                    APP,
                    "finance",
                    user,
                    SubjectKind::Human,
                ))
                .with_origin(origin.clone());
            ApprovalRequest::new(
                id,
                ApprovalScope::new(APP, user, "s2"),
                &call,
                ApprovalPolicy::new([ApproverSelector::human(user)]),
            )
            .with_acting(ActingContext::derived(
                APP,
                "finance",
                user,
                SubjectKind::Human,
            ))
        };

        // Three of Finance's requests wait for Ben: none waits for Ana.
        let (store, registry) = fresh().await;
        let origin = ToolOrigin::of(tool(&registry, "verb_snooze").as_ref());
        for index in 0..3 {
            store.create_request(&pending(format!("ben-{index}"), "ben", &origin)).await.unwrap();
        }
        let mut call = PolicyCall::new();
        call.origin = Some(&origin);
        let policy = registry.policy(PartitionId::default());
        let decision = policy.evaluate_call(&request_for(args.clone(), "s1"), &call).await;
        assert_eq!(decided(decision), Decided::Ask(move_text.clone()), "another person's requests");

        // A hundred older requests come first; Finance's three still count.
        let (store, registry) = fresh().await;
        let other = ToolOrigin::new("mcp:calendar", &json!({}), None);
        for index in 0..100 {
            store.create_request(&pending(format!("a-{index:03}"), OWNER, &other)).await.unwrap();
        }
        for index in 0..3 {
            store.create_request(&pending(format!("v-{index}"), OWNER, &origin)).await.unwrap();
        }
        let policy = registry.policy(PartitionId::default());
        let decision = policy.evaluate_call(&request_for(args.clone(), "s1"), &call).await;
        assert_eq!(
            decided(decision),
            Decided::Deny("Finance already has 3 changes waiting for you".into()),
            "past the first page"
        );

        // Ana said yes to this call; it resumes while three others wait.
        let (store, registry) = fresh().await;
        for index in 0..3 {
            store.create_request(&pending(format!("v-{index}"), OWNER, &origin)).await.unwrap();
        }
        let policy = registry.policy(PartitionId::default());
        let decision = policy.evaluate_call(&request_for(args.clone(), "s1"), &call).await;
        assert!(matches!(decided(decision), Decided::Deny(_)), "a new request is refused");
        let held = ToolConfirmationRequest::new("verb_snooze", args.clone())
            .with_acting_context(ActingContext::derived(APP, "finance", OWNER, SubjectKind::Human))
            .with_origin(origin.clone());
        store
            .decide(
                &ApprovalScope::new(APP, OWNER, "s1"),
                &held.fingerprint(),
                ToolApproval::approve(),
            )
            .await
            .unwrap();
        let decision = policy.evaluate_call(&request_for(args.clone(), "s1"), &call).await;
        assert_eq!(decided(decision), Decided::Ask(move_text.clone()), "a resumed call");

        // Ana edited another call to these arguments; it runs with them while three wait.
        let mut edited = pending("edited".into(), OWNER, &origin);
        edited.scope = ApprovalScope::new(APP, OWNER, "s3");
        edited.state = ApprovalState::Approved;
        edited.decisions.push(ApprovalDecision::edit(
            adk_core::principal::SubjectRef::human(OWNER),
            args.clone(),
        ));
        store.create_request(&edited).await.unwrap();
        let decision = policy.evaluate_call(&request_for(args.clone(), "s3"), &call).await;
        assert_eq!(decided(decision), Decided::Ask(move_text), "an edited call");
    }

    /// The person's and the agent's calls carry one declaration, so a decision's origin digest
    /// is the same whoever calls.
    #[test]
    fn the_tools_are_classed_by_their_effects() {
        use adk_harness::OriginClasses;
        let registry = memory_registry(&MemoryAgenda::default()).build().unwrap();
        let classes = registry.classes();
        assert_eq!(
            classes.class_of("verbs:adk-rust", "verb_snooze"),
            Some(adk_harness::driver_tool_kind(EffectClass::WriteLocal))
        );
        assert_eq!(
            classes.class_of("verbs:adk-rust", "verb_coming_up"),
            Some(adk_harness::driver_tool_kind(EffectClass::Read))
        );
        assert_eq!(classes.class_of("mcp:other", "verb_snooze"), None);
        assert_eq!(classes.class_of("verbs:adk-rust", "verb_unknown"), None);
        let snooze = tool(&registry, "verb_snooze");
        assert_eq!(snooze.origin(), "verbs:adk-rust");
        assert!(!snooze.is_read_only());
        assert!(tool(&registry, "verb_coming_up").is_read_only());
    }
}

mod snoozing {
    use super::*;

    async fn refusal(agenda: &MemoryAgenda, args: Value) -> String {
        let registry = memory_registry(agenda).build().unwrap();
        let tool = registry
            .toolset(PartitionId::default())
            .tools()
            .into_iter()
            .find(|tool| tool.name() == "verb_snooze")
            .unwrap();
        let _ = tool;
        // The handler's own refusal, as the policy reads it before any card.
        let policy = registry.policy(PartitionId::default());
        let origin = ToolOrigin::new("verbs:adk-rust", &json!({}), None);
        let mut request = ToolPolicyRequest::new("verb_snooze", args);
        request.user_id = OWNER.into();
        request.agent_name = "finance".into();
        request.acting = Some(ActingContext::derived(APP, "finance", OWNER, SubjectKind::Human));
        let mut call = PolicyCall::new();
        call.origin = Some(&origin);
        match policy.evaluate_call(&request, &call).await {
            PolicyDecision::Deny { reason } => reason,
            other => panic!("not refused: {other:?}"),
        }
    }

    /// Spec 3.3 §2 item 7: snooze moves only a scheduled item waiting for one time, whose
    /// process has not started, to an hour later or the same time tomorrow, before it ends.
    #[tokio::test]
    async fn snooze_refuses_what_it_cannot_move() {
        let at = digest_at();
        let to = |at: chrono::DateTime<Utc>| at.with_timezone(&offset()).to_rfc3339();
        let args = |id: &str| json!({ "item": id, "to": to(at + Span::hours(1)) });
        let agenda = MemoryAgenda::default();
        let base = scheduled("x", "News digest", OWNER, "default", at);
        // What, its state, when it ends, whether it started, and the refusal.
        type Case = (&'static str, AgendaState, Option<chrono::DateTime<Utc>>, bool, &'static str);
        let cases: Vec<Case> = vec![
            ("proposed", AgendaState::Proposed, None, false, "it isn't scheduled yet"),
            ("approved", AgendaState::Approved, None, false, "it isn't scheduled yet"),
            (
                "running",
                AgendaState::Running { pid: "run-1".into() },
                None,
                false,
                "it has already started",
            ),
            (
                "recurring",
                AgendaState::Scheduled {
                    wake: WakeSet::any_of([Wake::Cron {
                        expr: "0 18 * * *".into(),
                        missed: adk_core::schedule::MissedFirePolicy::Skip,
                    }]),
                },
                None,
                false,
                "it doesn't wait for one time, so it can't be snoozed",
            ),
            (
                "two wakes",
                AgendaState::Scheduled { wake: WakeSet::any_of([Wake::At(at), Wake::Manual]) },
                None,
                false,
                "it doesn't wait for one time, so it can't be snoozed",
            ),
            ("queued", base.state.clone(), None, true, "it has already started"),
            ("ends first", base.state.clone(), Some(at + Span::minutes(30)), false, ""),
        ];
        for (id, state, ends, started, expected) in cases {
            let mut item = base.clone();
            item.id = id.into();
            item.state = state;
            item.expires_at = ends;
            agenda.put(item);
            if started {
                agenda.start(id);
            }
            let expected = if expected.is_empty() {
                format!(
                    "It ends at {}, so it can't move past then",
                    time_text(at + Span::minutes(30))
                )
            } else {
                expected.to_string()
            };
            assert_eq!(refusal(&agenda, args(id)).await, expected, "{id}");
            assert_eq!(agenda.get(id).unwrap().state, agenda.get(id).unwrap().state);
        }
        // A `to` that is not one of its two times, as an Edit can give it, names the two; one
        // it is already as late as means it moved since the request was made.
        let mut moved = base.clone();
        moved.id = "moved".into();
        agenda.put(moved);
        let edited = json!({ "item": "moved", "to": to(at + Span::minutes(90)) });
        assert_eq!(
            refusal(&agenda, edited).await,
            format!(
                "It can only move to {} or {}",
                time_text(at + Span::hours(1)),
                time_text(at + Span::days(1))
            )
        );
        let stale = json!({ "item": "moved", "to": to(at - Span::hours(1)) });
        assert_eq!(refusal(&agenda, stale).await, "It moved since you saw this");
        assert_eq!(
            refusal(&agenda, json!({ "item": "moved" })).await,
            "snooze takes `item`, the id coming_up lists, and `to`, one of its two times"
        );
    }

    /// An agenda whose every other "has it started" check waits, briefly, for the next one:
    /// two snoozes check the same wake together, as two surfaces can.
    struct Racing {
        inner: MemoryAgenda,
        checks: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl adk_surface::verbs::AgendaAccess for Racing {
        async fn items(
            &self,
            subject: &str,
            area: Option<&PartitionId>,
        ) -> Result<Vec<adk_kernel::AgendaItem>, VerbError> {
            self.inner.items(subject, area).await
        }

        async fn item(&self, id: &str) -> Result<Option<adk_kernel::AgendaItem>, VerbError> {
            self.inner.item(id).await
        }

        async fn started(&self, item: &adk_kernel::AgendaItem) -> Result<bool, VerbError> {
            use std::sync::atomic::Ordering;
            let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
            if check % 2 == 1 {
                let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
                while self.checks.load(Ordering::SeqCst) <= check
                    && tokio::time::Instant::now() < deadline
                {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
            self.inner.started(item).await
        }

        async fn reschedule(
            &self,
            id: &str,
            wake: WakeSet,
            by: &adk_core::principal::SubjectRef,
        ) -> Result<adk_kernel::AgendaItem, VerbError> {
            self.inner.reschedule(id, wake, by).await
        }
    }

    /// Two snoozes of one item at once, "1 hour later" in the home and "Tomorrow, same time"
    /// in the terminal: one moves it, the other is refused — never both passing their checks
    /// against the same wake, so the later one cannot move the wake back (spec 3.3 §2 item 7).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_snoozes_at_once_move_the_item_once() {
        let home =
            home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new())))).await;
        let agenda = MemoryAgenda::default();
        let at = digest_at();
        agenda.put(scheduled(DIGEST, "News digest", OWNER, "default", at));
        let racing = Racing { inner: agenda.clone(), checks: Default::default() };
        let registry = adk_surface::verbs::VerbRegistry::builder(APP, Arc::new(racing))
            .utc_offset(offset())
            .build()
            .unwrap();
        let gate = adk_surface::verbs::VerbGate::new(registry, home.engine.clone());
        let (home_app, keyboard) = (web(), terminal());
        let (hour, day) = tokio::join!(
            gate.call(&home_app, snooze(DIGEST, at + Span::hours(1))),
            gate.call(&keyboard, snooze(DIGEST, at + Span::days(1))),
        );
        let moved: Vec<_> = [&hour, &day].into_iter().filter(|done| done.is_ok()).collect();
        assert_eq!(moved.len(), 1, "{hour:?} {day:?}");
        let refused = [hour.as_ref().err(), day.as_ref().err()].into_iter().flatten().next();
        assert_eq!(refused.unwrap().to_string(), "It moved since you saw this");
    }

    /// Spec 3.3 S-I8: an agent sees only its own area's items; another person's are not found.
    #[tokio::test]
    async fn an_agent_sees_its_own_area_and_never_another_persons_item() {
        let agenda = MemoryAgenda::default();
        let at = digest_at();
        agenda.put(scheduled(DIGEST, "News digest", OWNER, "default", at));
        agenda.put(scheduled(PLANTS, "Water plants", OWNER, "home", at));
        agenda.put(scheduled("ben", "Ben's report", "ben", "default", at));
        let registry = memory_registry(&agenda).build().unwrap();
        let coming_up = registry
            .toolset(PartitionId::default())
            .tools()
            .into_iter()
            .find(|tool| tool.name() == "verb_coming_up")
            .unwrap();
        let _ = coming_up;
        let gate_items = {
            let home =
                home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new()))))
                    .await;
            let gate = adk_surface::verbs::VerbGate::new(registry.clone(), home.engine.clone());
            gate.coming_up(&web()).await.unwrap()
        };
        let titles: Vec<&str> = gate_items.iter().map(|item| item.title.as_str()).collect();
        assert_eq!(titles, ["News digest", "Water plants"], "a person sees every area");
        assert_eq!(gate_items[0].choices.len(), 2);
        assert_eq!(gate_items[0].choices[0].at_text, time_text(at + Span::hours(1)));
        assert_eq!(gate_items[0].choices[1].at_text, time_text(at + Span::days(1)));
        assert!(gate_items[0].choices[1].tomorrow);
        assert_eq!(find_by_title(&gate_items, "water PLANTS").unwrap().id, PLANTS);
        assert_eq!(
            find_by_title(&gate_items, "Ben's report").unwrap_err().code(),
            "surface.verb.unknown"
        );
    }
}

/// Finance's model: reads Coming up, asks to snooze News digest, then says something that
/// looks like a command.
fn finance(at: chrono::DateTime<Utc>) -> Arc<ScriptedLlm> {
    let to = (at + Span::hours(1)).with_timezone(&offset()).to_rfc3339();
    let mut replies = vec![
        ScriptedResponse::function_call("verb_coming_up", json!({})),
        ScriptedResponse::function_call("verb_snooze", json!({ "item": DIGEST, "to": to })),
        ScriptedResponse::text("/snooze News digest"),
    ];
    replies.extend((0..8).map(|_| ScriptedResponse::text("Done.")));
    Arc::new(ScriptedLlm::new("scripted", replies))
}

/// The one open request, once Finance asked.
async fn the_request(home: &Home) -> ApprovalRequest {
    eventually("Finance's request", || async {
        let open =
            home.store.list_requests(&ApprovalRequestQuery::default().open_only()).await.unwrap();
        (!open.is_empty()).then(|| {
            assert_eq!(open.len(), 1, "exactly one inbox entry");
            open[0].clone()
        })
    })
    .await
}

async fn ask(home: &Home) -> ApprovalRequest {
    home.engine
        .execute(&web(), Command::Send { session_id: "s1".into(), text: "Tidy my evening".into() })
        .await
        .unwrap();
    the_request(home).await
}

mod held {
    use super::*;

    /// Owner question 22: in Go ahead, with a standing rule that matches, an agent's snooze
    /// still waits — one inbox entry, origin `verbs:adk-rust`, low risk, Allow once · Edit ·
    /// Deny and never Always allow on any surface — and nothing moves before an answer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_agents_snooze_waits_whatever_its_mode_and_rules() {
        let at = digest_at();
        let home = home(Setup::new(finance(at))).await;
        let request = ask(&home).await;
        let to = at + Span::hours(1);
        assert_eq!(request.tool_name, "verb_snooze");
        assert_eq!(request.origin.as_ref().unwrap().source, "verbs:adk-rust");
        assert_eq!(
            request.acting.as_ref().unwrap().agent.id.as_str(),
            "urn:adk:agent:adk-rust/finance"
        );
        assert_eq!(request.risk, ApprovalRisk::Low);
        assert_eq!(request.summary, format!("Move News digest to {}", time_text(to)));
        assert_eq!(
            request.allowed_actions,
            [ApprovalAction::Approve, ApprovalAction::Edit, ApprovalAction::Reject]
        );
        assert_eq!(home.wake_of(DIGEST).await, Some(at), "nothing moves before an answer");
        // The governed path's record: held, despite Go ahead and the rule; the read allowed.
        let held = home.tool_records("verb_snooze");
        assert_eq!(held.len(), 1, "{held:?}");
        assert_eq!(held[0].decision, Some(AuditDecision::ApprovalRequired));
        assert_eq!(held[0].ext["toolSource"], json!("verbs:adk-rust"));
        let read = home.tool_records("verb_coming_up");
        assert_eq!(read[0].decision, Some(AuditDecision::Allowed));
        // Every surface's card.
        let answers = |card: &adk_surface::DecisionCard| -> Vec<Answer> {
            card.allowed.options.iter().map(|option| option.answer).collect()
        };
        for (caller, expected) in [
            (web(), vec![Answer::AllowOnce, Answer::Edit, Answer::Deny]),
            (phone(), vec![Answer::AllowOnce, Answer::Edit, Answer::Deny]),
            (terminal(), vec![Answer::AllowOnce, Answer::Edit, Answer::Deny]),
            (tray(), vec![Answer::AllowOnce, Answer::Deny]),
            (telegram(), vec![Answer::AllowOnce, Answer::Deny]),
        ] {
            let card = home.engine.decision(&caller, &request.id).await.unwrap();
            assert_eq!(answers(&card), expected, "{:?}", caller.surface);
            assert_eq!(card.reversibility_text, "You can change it back");
            assert_eq!(card.risk_text, "Low risk");
            if caller.surface == adk_surface::SurfaceKind::Chat {
                assert_eq!(card.heading, "Snooze something in Coming up");
                assert_eq!(card.call.args, Value::Null, "no arguments in chat (S-I5)");
                assert!(card.detail.is_none());
            } else {
                assert_eq!(card.heading, request.summary);
                assert_eq!(
                    card.detail.as_ref().map(|detail| detail.text.as_str()),
                    Some("Agents always ask before they change things on your screens.")
                );
            }
        }
        // The agent's '/snooze' in its reply is text: no record, nothing moved.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(home.verb_records().is_empty());
        assert_eq!(home.wake_of(DIGEST).await, Some(at));
    }

    /// Spec 3.3 §12 step 5: Allow once on the phone wakes Finance's parked run, the item
    /// moves exactly once, and the record names Finance, the call and the decision; a second
    /// answer reads "already decided".
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn allow_once_on_the_phone_moves_it_once() {
        let at = digest_at();
        let home = home(Setup::new(finance(at))).await;
        let request = ask(&home).await;
        let to = at + Span::hours(1);
        let answer = |version| Command::Decide {
            decision_id: request.id.clone(),
            version,
            answer: Answer::AllowOnce,
            edited_args: None,
            reason: None,
            expanded: true,
        };
        let outcome = home.engine.execute(&phone(), answer(request.version)).await.unwrap();
        assert!(matches!(outcome, CommandOutcome::Answered { .. }), "{outcome:?}");
        eventually("the snooze", || async {
            (home.wake_of(DIGEST).await == Some(to)).then_some(())
        })
        .await;
        let records = eventually("its record", || async {
            let records = home.verb_records();
            (!records.is_empty()).then_some(records)
        })
        .await;
        assert_eq!(records.len(), 1, "{records:?}");
        let record = &records[0];
        assert_eq!(record.ext["origin"], json!({ "kind": "agent", "agent": "finance" }));
        assert_eq!(record.ext["verb"], json!("snooze"));
        assert_eq!(record.ext["target"], json!(format!("agenda:{DIGEST}")));
        assert_eq!(record.ext["title"], json!("News digest"));
        assert_eq!(record.ext["outcome"], json!("done"));
        assert_eq!(record.ext["effect"], json!("write_local"));
        assert_eq!(record.ext["approvalId"], json!(request.id));
        assert_eq!(record.ext["answered"]["surface"], json!("phone"));
        assert_eq!(record.ext["answered"]["origin"], json!({ "kind": "touch" }));
        assert!(record.ext["functionCallId"].is_string());
        assert_eq!(
            record.actor.agent.as_ref().map(|agent| agent.as_str()),
            Some("urn:adk:agent:adk-rust/finance")
        );
        let again = home.engine.execute(&phone(), answer(request.version)).await.unwrap();
        assert!(matches!(again, CommandOutcome::AlreadyDecided { .. }), "{again:?}");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(home.wake_of(DIGEST).await, Some(to), "exactly once");
    }

    /// Spec 3.3 §12 step 5 twins: a denial changes nothing and writes no record of a change;
    /// a snooze the person made first leaves Finance's allowed request refused as moved.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn deny_changes_nothing_and_a_moved_item_is_refused() {
        let at = digest_at();
        let home = home(Setup::new(finance(at))).await;
        let request = ask(&home).await;
        home.engine
            .execute(
                &web(),
                Command::Decide {
                    decision_id: request.id.clone(),
                    version: request.version,
                    answer: Answer::Deny,
                    edited_args: None,
                    reason: None,
                    expanded: true,
                },
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(home.wake_of(DIGEST).await, Some(at));
        assert!(home.verb_records().iter().all(|record| record.ext["outcome"] != "done"));

        let at = digest_at();
        let home = super::verbs_support::home(Setup::new(finance(at))).await;
        let request = ask(&home).await;
        // Ana snoozes it herself first.
        home.gate.call(&web(), snooze(DIGEST, at + Span::hours(1))).await.unwrap();
        home.engine
            .execute(
                &phone(),
                Command::Decide {
                    decision_id: request.id.clone(),
                    version: request.version,
                    answer: Answer::AllowOnce,
                    edited_args: None,
                    reason: None,
                    expanded: true,
                },
            )
            .await
            .unwrap();
        eventually("the refusal", || async {
            home.tool_records("verb_snooze")
                .iter()
                .any(|record| record.decision == Some(AuditDecision::Denied))
                .then_some(())
        })
        .await;
        assert_eq!(home.wake_of(DIGEST).await, Some(at + Span::hours(1)), "moved once, by ana");
        let done: Vec<_> = home
            .verb_records()
            .into_iter()
            .filter(|record| record.ext["outcome"] == "done")
            .collect();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].ext["surface"], json!("web"));
    }

    /// Finance's call, as the runtime hands it to the tool.
    struct FinanceCall {
        content: adk_core::Content,
        actions: std::sync::Mutex<adk_core::EventActions>,
    }

    impl adk_core::ReadonlyContext for FinanceCall {
        fn invocation_id(&self) -> &str {
            "inv-1"
        }
        fn agent_name(&self) -> &str {
            "finance"
        }
        fn user_id(&self) -> &str {
            OWNER
        }
        fn app_name(&self) -> &str {
            APP
        }
        fn session_id(&self) -> &str {
            "s1"
        }
        fn branch(&self) -> &str {
            ""
        }
        fn user_content(&self) -> &adk_core::Content {
            &self.content
        }
    }

    impl adk_core::CallbackContext for FinanceCall {
        fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
            None
        }
    }

    #[async_trait::async_trait]
    impl adk_core::ToolContext for FinanceCall {
        fn function_call_id(&self) -> &str {
            "call-a"
        }
        fn actions(&self) -> adk_core::EventActions {
            self.actions.lock().unwrap().clone()
        }
        fn set_actions(&self, actions: adk_core::EventActions) {
            *self.actions.lock().unwrap() = actions;
        }
        async fn search_memory(&self, _: &str) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
            Ok(Vec::new())
        }
    }

    /// Spec 3.3 §11: an agent's record cites the decision that let this very call run, never
    /// the one resolved last for another call of the same tool in the session.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_record_cites_the_approval_of_this_call() {
        use adk_core::ApprovalStore as _;
        let home =
            home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new())))).await;
        let tool = home
            .registry
            .toolset(PartitionId::default())
            .tools()
            .into_iter()
            .find(|tool| tool.name() == "verb_snooze")
            .unwrap();
        let to = |at: chrono::DateTime<Utc>| at.with_timezone(&offset()).to_rfc3339();
        let finance = ActingContext::derived(APP, "finance", OWNER, SubjectKind::Human);
        let approved = |id: &str, args: Value, minute: u32| {
            let call = ToolConfirmationRequest::for_tool(tool.as_ref(), args)
                .with_acting_context(finance.clone());
            let mut request = ApprovalRequest::new(
                id,
                ApprovalScope::new(APP, OWNER, "s1"),
                &call,
                ApprovalPolicy::new([ApproverSelector::human(OWNER)]),
            )
            .with_acting(finance.clone());
            request.state = ApprovalState::Approved;
            request.resolved_at = Some(Utc::now() + Span::minutes(i64::from(minute)));
            request
        };
        // Ana said yes to this call first, then to another call of Finance's.
        let this = json!({ "item": DIGEST, "to": to(home.at + Span::hours(1)) });
        let other = json!({ "item": DIGEST, "to": to(home.at + Span::days(1)) });
        home.store.create_request(&approved("apr-this", this.clone(), 1)).await.unwrap();
        home.store.create_request(&approved("apr-other", other, 2)).await.unwrap();
        let ctx = Arc::new(FinanceCall {
            content: adk_core::Content::new("user"),
            actions: Default::default(),
        });
        tool.execute(ctx, this).await.unwrap();
        let record = home.verb_records().pop().unwrap();
        assert_eq!(record.ext["outcome"], json!("done"));
        assert_eq!(record.ext["approvalId"], json!("apr-this"));
    }

    /// Owner question 23: while everything is stopped, a person's snooze still runs — it only
    /// delays work — and an agent asks nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stop_all_lets_a_person_snooze_and_stops_the_agent() {
        let at = digest_at();
        let home = home(Setup::new(finance(at))).await;
        home.engine.execute(&web(), Command::StopAll { reason: None }).await.unwrap();
        let done = home.gate.call(&web(), snooze(DIGEST, at + Span::hours(1))).await.unwrap();
        assert_eq!(done.title.as_deref(), Some("News digest"));
        assert_eq!(home.wake_of(DIGEST).await, Some(at + Span::hours(1)));
        let _ = home
            .engine
            .execute(
                &web(),
                Command::Send { session_id: "s1".into(), text: "Tidy my evening".into() },
            )
            .await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let open =
            home.store.list_requests(&ApprovalRequestQuery::default().open_only()).await.unwrap();
        assert!(open.is_empty(), "no card while stopped: {open:?}");
    }

    /// A person's action that does more than delay waits for Resume all.
    #[tokio::test]
    async fn a_person_action_that_does_more_than_delay_waits_while_stopped() {
        struct Pin;
        #[async_trait::async_trait]
        impl VerbHandler for Pin {
            async fn describe(&self, _: &VerbScope, _: &Value) -> Result<String, VerbError> {
                Ok("Pin".into())
            }
            async fn run(&self, _: &VerbScope, _: &Value) -> Result<VerbDone, VerbError> {
                Ok(VerbDone::new("pin", "Pinned"))
            }
        }
        let home =
            home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new())))).await;
        let registry = memory_registry(&MemoryAgenda::default())
            .governance(home.governance.clone())
            .verb(VerbSpec::new("pin", "Pin", "Pin", EffectClass::WriteLocal), Arc::new(Pin))
            .build()
            .unwrap();
        let gate = adk_surface::verbs::VerbGate::new(registry, home.engine.clone());
        home.governance.freeze("test");
        let refused = gate.call(&web(), VerbIntent::new("pin", json!({}))).await.unwrap_err();
        assert_eq!(refused.code(), "surface.verb.refused");
        home.governance.unfreeze();
        assert_eq!(
            gate.call(&web(), VerbIntent::new("pin", json!({}))).await.unwrap().text,
            "Pinned"
        );
    }
}

mod person {
    use super::*;

    /// Spec 3.3 §12 step 1–2: a person's snooze moves the wake, writes one `surface.verb`
    /// record stamped with where and how, and one `tool_call` allowed under
    /// `urn:adk:agent:adk-rust/surface.verbs` acting for the person.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_persons_snooze_is_governed_and_recorded() {
        let home =
            home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new())))).await;
        let to = home.at + Span::hours(1);
        let done = home.gate.call(&web(), snooze(DIGEST, to)).await.unwrap();
        assert_eq!(done.text, format!("Moved News digest to {}", time_text(to)));
        assert_eq!(home.wake_of(DIGEST).await, Some(to));
        let records = home.verb_records();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.ext["surface"], json!("web"));
        assert_eq!(record.ext["origin"], json!({ "kind": "pointer" }));
        assert_eq!(record.ext["target"], json!(format!("agenda:{DIGEST}")));
        assert_eq!(record.ext["outcome"], json!("done"));
        assert_eq!(record.actor.on_behalf_of.as_ref().unwrap().id, OWNER);
        let calls = home.tool_records("verb_snooze");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].decision, Some(AuditDecision::Allowed));
        assert_eq!(
            calls[0].actor.agent.as_ref().map(|agent| agent.as_str()),
            Some("urn:adk:agent:adk-rust/surface.verbs")
        );
        assert_eq!(calls[0].actor.on_behalf_of.as_ref().unwrap().id, OWNER);
        assert_eq!(calls[0].ext["toolSource"], json!("verbs:adk-rust"));
        // The same request again: it moved since; its record names the item by its title.
        let again = home.gate.call(&web(), snooze(DIGEST, to)).await.unwrap_err();
        assert_eq!(again.to_string(), "It moved since you saw this");
        let refused = home.verb_records().pop().unwrap();
        assert_eq!(refused.ext["outcome"], json!("refused"));
        assert_eq!(refused.ext["title"], json!("News digest"));
        // A time of the person's own choosing is not one of the two.
        let edited = home.gate.call(&web(), snooze(DIGEST, to + Span::minutes(30))).await;
        assert_eq!(
            edited.unwrap_err().to_string(),
            format!(
                "It can only move to {} or {}",
                time_text(to + Span::hours(1)),
                time_text(to + Span::days(1))
            )
        );
    }

    /// Spec 3.3 §7.2, owner question 27: chat and the tray take no screen action.
    #[tokio::test]
    async fn chat_and_the_tray_take_no_screen_action() {
        let home =
            home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new())))).await;
        for caller in [telegram(), tray()] {
            let refused = home.gate.call(&caller, snooze(DIGEST, home.at + Span::hours(1))).await;
            assert_eq!(
                refused.unwrap_err().code(),
                "surface.verb.not_here",
                "{:?}",
                caller.surface
            );
        }
        assert_eq!(home.wake_of(DIGEST).await, Some(home.at));
    }
}

mod web_routes {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn call(
        router: &axum::Router,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn device(kind: Option<&str>) -> RequestContext {
        let mut context = owner_context();
        context.metadata.insert(adk_kernel::pairing::DEVICE_ID_METADATA.into(), "dev-1".into());
        if let Some(kind) = kind {
            context.metadata.insert(adk_kernel::pairing::DEVICE_KIND_METADATA.into(), kind.into());
        }
        context
    }

    fn body(origin: Value, id: &str, to: chrono::DateTime<Utc>) -> Value {
        json!({
            "origin": origin,
            "intent": {
                "verb": "snooze",
                "objects": [{ "kind": "comingUp", "id": id }],
                "args": { "to": to.with_timezone(&offset()).to_rfc3339() },
            },
        })
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_routes_answer_only_the_authenticated_person() {
        let home =
            home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new())))).await;
        let to = home.at + Span::hours(1);
        let api = home.api(Some(owner_context()));
        let (status, listed) = call(&api, "GET", "/api/surface/verbs", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed["verbs"].as_array().unwrap().len(), 2);
        assert_eq!(listed["comingUp"][0]["title"], json!("News digest"));
        assert_eq!(listed["comingUp"][0]["choices"][0]["label"], json!("1 hour later"));
        assert_eq!(listed["comingUp"][0]["choices"][1]["label"], json!("Tomorrow, same time"));

        let anonymous = home.api(None);
        assert_eq!(
            call(&anonymous, "GET", "/api/surface/verbs", None).await.0,
            StatusCode::UNAUTHORIZED
        );
        let pointer = json!({ "kind": "pointer" });
        assert_eq!(
            call(
                &anonymous,
                "POST",
                "/api/surface/verbs/call",
                Some(body(pointer.clone(), DIGEST, to))
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
        // Spoken and agent origins come through their own paths.
        for origin in [json!({ "kind": "voice" }), json!({ "kind": "agent", "agent": "finance" })] {
            let (status, problem) =
                call(&api, "POST", "/api/surface/verbs/call", Some(body(origin, DIGEST, to))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
            assert_eq!(problem["error"]["code"], json!("surface.verb.invalid"));
        }
        // Someone else's item, or none, is not found.
        let (status, _) =
            call(&api, "POST", "/api/surface/verbs/call", Some(body(pointer.clone(), "nope", to)))
                .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // A terminal paired from another machine cannot; a phone can.
        let remote = home.api(Some(device(None)));
        let (status, problem) = call(
            &remote,
            "POST",
            "/api/surface/verbs/call",
            Some(body(pointer.clone(), DIGEST, to)),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
        assert_eq!(home.wake_of(DIGEST).await, Some(home.at));
        let phone = home.api(Some(device(Some("phone"))));
        let (status, done) = call(
            &phone,
            "POST",
            "/api/surface/verbs/call",
            Some(body(json!({ "kind": "touch" }), DIGEST, to)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{done}");
        assert_eq!(done["title"], json!("News digest"));
        assert_eq!(home.wake_of(DIGEST).await, Some(to));
        let record = home.verb_records().pop().unwrap();
        assert_eq!(record.ext["surface"], json!("phone"));
        assert_eq!(record.ext["device"], json!("dev-1"));
        let (status, document) = call(&api, "GET", "/api/surface/verbs/openapi.json", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(document["paths"]["/api/surface/verbs/call"].is_object());
    }

    /// The kernel's surface-records route takes a `surface.verb` record from an attached
    /// engine (spec 3.3 §11).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_kernel_takes_a_surface_verb_record() {
        let home =
            home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new())))).await;
        let api = home.api(Some(owner_context()));
        let record = json!({
            "kind": "surface",
            "action": "surface.verb",
            "ext": { "surface": "terminal", "origin": { "kind": "keyboard" }, "verb": "snooze", "outcome": "done" },
        });
        let (status, answer) =
            call(&api, "POST", "/api/kernel/surface/records", Some(record)).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer, json!({ "recorded": true }));
        let written = home.verb_records().pop().unwrap();
        assert_eq!(written.ext["verb"], json!("snooze"));
    }
}

#[cfg(feature = "tui")]
mod terminal_command {
    use super::*;
    use adk_surface::tui::{CommandContext, TerminalCommand, TerminalSurface};
    use adk_surface::verbs::SnoozeCommand;

    /// Spec 3.3 §5.2: `/snooze <title> [tomorrow]` as the terminal's caller — by exact title,
    /// ignoring case; several matches listed; none said so; a refusal naming the item.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn snooze_by_title_in_the_terminal() {
        let home =
            home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new())))).await;
        home.schedule("weekly", "Weekly digest", PartitionId::default(), home.at + Span::hours(2))
            .await;
        let command = SnoozeCommand::new(home.gate.clone(), terminal());
        let context = CommandContext::new("s1");
        let done = command.run("news DIGEST", &context).await.unwrap();
        assert_eq!(
            done,
            format!("✓ snoozed News digest → {}", time_text(home.at + Span::hours(1)))
        );
        let ambiguous = command.run("digest", &context).await.unwrap_err();
        assert_eq!(
            ambiguous.message,
            "two things match “digest” — type the full title:\n  News digest\n  Weekly digest"
        );
        let none = command.run("laundry", &context).await.unwrap_err();
        assert_eq!(none.message, "nothing in Coming up is called “laundry”");
        let tomorrow = command.run("Water plants tomorrow", &context).await.unwrap();
        assert_eq!(
            tomorrow,
            format!(
                "✓ snoozed Water plants → {}",
                time_text(home.at + Span::hours(1) + Span::days(1))
            )
        );
        let record = home.verb_records().pop().unwrap();
        assert_eq!(record.ext["surface"], json!("terminal"));
        assert_eq!(record.ext["origin"], json!({ "kind": "keyboard" }));
        let remote = SnoozeCommand::remote().run("News digest", &context).await.unwrap_err();
        assert_eq!(remote.message, "Snooze isn't available from a remote terminal — use the home.");
    }

    /// `--output json`: a screen action typed as the message is one `verb` line.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_screen_action_is_one_json_line() {
        let home =
            home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new())))).await;
        let surface = TerminalSurface::new(home.engine.clone(), terminal())
            .command(Arc::new(SnoozeCommand::new(home.gate.clone(), terminal())));
        let mut out = Vec::new();
        let code = surface.run_json(Some("/snooze News digest".into()), &mut out).await.unwrap();
        assert_eq!(code, 0);
        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines[0]["type"], json!("verb"));
        assert_eq!(lines[0]["verb"], json!("snooze"));
        assert_eq!(lines[0]["outcome"], json!("done"));
        assert_eq!(lines[0]["how"], json!("keyboard"));
        assert_eq!(home.wake_of(DIGEST).await, Some(home.at + Span::hours(1)));
    }
}

mod card_hints {
    use super::*;
    use adk_core::{Content, Event, FunctionResponseData, Part};
    use adk_session::CreateRequest;
    use adk_surface::CardHints;

    fn call(id: &str, name: &str, args: Value) -> Part {
        Part::FunctionCall { name: name.into(), args, id: Some(id.into()), thought_signature: None }
    }

    fn response(id: &str, name: &str, response: Value) -> Part {
        Part::FunctionResponse {
            function_response: FunctionResponseData::new(name, response),
            id: Some(id.into()),
            annotations: None,
        }
    }

    /// A page `web_fetch` fetched from `url`, as the tool answers.
    fn page(url: &str) -> Value {
        json!({ "url": url, "final_url": url, "status": 200, "bytes": 812 })
    }

    /// Writes the turns `parts` of `author` to session `session`, and the held request for
    /// its `verb_snooze` call.
    async fn asked(
        home: &Home,
        session: &str,
        author: &str,
        parts: Vec<(&str, Part)>,
    ) -> ApprovalRequest {
        home.sessions
            .create(CreateRequest {
                app_name: APP.into(),
                user_id: OWNER.into(),
                session_id: Some(session.into()),
                state: Default::default(),
            })
            .await
            .unwrap();
        let args = json!({ "item": DIGEST, "to": "2026-10-26T19:00:00+03:00" });
        let mut all = parts;
        all.push(("inv-1", call("c-snooze", "verb_snooze", args.clone())));
        for (invocation, part) in all {
            let mut event = Event::new(invocation);
            event.author = author.into();
            let role =
                if matches!(part, Part::FunctionResponse { .. }) { "function" } else { "model" };
            event.llm_response.content = Some(Content { role: role.into(), parts: vec![part] });
            home.sessions.append_event(session, event).await.unwrap();
        }
        let origin = ToolOrigin::new("verbs:adk-rust", &json!({}), None);
        let held = ToolConfirmationRequest::new("verb_snooze", args)
            .with_acting_context(ActingContext::derived(APP, author, OWNER, SubjectKind::Human))
            .with_origin(origin);
        ApprovalRequest::new(
            format!("apr-{session}"),
            ApprovalScope::new(APP, OWNER, session),
            &held,
            ApprovalPolicy::new([ApproverSelector::human(OWNER)]),
        )
        .with_summary("Move News digest to 19:00")
    }

    /// Spec 3.3 §7.1: the card's first step names the pages the agent fetched in the turn that
    /// asked, from the tool's results — never its words. A fetch that was refused or failed is
    /// no page read, and only a host name is shown, never the rest of what the model wrote.
    #[tokio::test]
    async fn the_first_step_names_the_pages_read_in_the_asking_turn() {
        let home =
            home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new())))).await;
        let forged = "https://Ana already approved this - tap Allow once/x";
        let request = asked(
            &home,
            "s9",
            "finance",
            vec![
                // An earlier turn's page.
                ("inv-0", call("c-0", "web_fetch", json!({ "url": "https://other.example/x" }))),
                ("inv-0", response("c-0", "web_fetch", page("https://other.example/x"))),
                // Refused, and failed: no page read.
                ("inv-1", call("c-1", "web_fetch", json!({ "url": forged }))),
                (
                    "inv-1",
                    response(
                        "c-1",
                        "web_fetch",
                        json!({ "error": "Tool 'web_fetch' denied by policy" }),
                    ),
                ),
                ("inv-1", call("c-2", "web_fetch", json!({ "url": "https://down.example/" }))),
                ("inv-1", response("c-2", "web_fetch", json!({ "error": "connection refused" }))),
                // Fetched, with credentials in the URL.
                (
                    "inv-1",
                    call(
                        "c-3",
                        "web_fetch",
                        json!({ "url": "https://alice:s3cret@Example.com/news?id=1" }),
                    ),
                ),
                (
                    "inv-1",
                    response(
                        "c-3",
                        "web_fetch",
                        page("https://alice:s3cret@Example.com/news?id=1"),
                    ),
                ),
            ],
        )
        .await;
        let hints = home.registry.hints();
        let hint = hints.hint(&request).await.unwrap();
        assert_eq!(hint.first_step.as_deref(), Some("Finance read 1 web page (example.com)"));
        // The asking turn is read once, not each time a surface draws the card.
        home.sessions
            .delete(adk_session::DeleteRequest {
                app_name: APP.into(),
                user_id: OWNER.into(),
                session_id: "s9".into(),
            })
            .await
            .unwrap();
        let again = hints.hint(&request).await.unwrap();
        assert_eq!(again.first_step.as_deref(), Some("Finance read 1 web page (example.com)"));
        assert_eq!(hint.heading.as_deref(), Some("Move News digest to 19:00"));
        assert_eq!(hint.chat_heading.as_deref(), Some("Snooze something in Coming up"));
        assert_eq!(hint.offer_always, Some(false));
        let other = ApprovalRequest::new(
            "apr-2",
            ApprovalScope::new(APP, OWNER, "s9"),
            &ToolConfirmationRequest::new("verb_snooze", json!({})),
            ApprovalPolicy::new([ApproverSelector::human(OWNER)]),
        );
        assert!(home.registry.hints().hint(&other).await.is_none(), "not a screen action's origin");
        // Only refused calls: nothing was read.
        let refused = asked(
            &home,
            "s10",
            "finance",
            vec![
                ("inv-1", call("c-1", "web_fetch", json!({ "url": forged }))),
                ("inv-1", response("c-1", "web_fetch", json!({ "error": "denied" }))),
            ],
        )
        .await;
        assert_eq!(home.registry.hints().hint(&refused).await.unwrap().first_step, None);
    }

    /// Spec 3.3 §5.7: read through the engine's source, the first step names an agent the
    /// person added as the card does, by the name the person gave it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_first_step_names_an_added_agent_as_the_card_does() {
        use adk_kernel::agents::{AgentDefinition, AgentOrigin, AgentRecord, AgentRecordState};
        let home =
            home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new())))).await;
        // Added as "money", named "Money Matters" by the person.
        let record = AgentRecord::new(
            "money",
            AgentDefinition::new("Money Matters", "default"),
            AgentRecordState::Active,
            AgentOrigin::ByHand,
            adk_core::principal::SubjectRef::human(OWNER),
            Utc::now(),
        );
        assert!(home.kernel.agents().store().put_if_version(record, 0).await.unwrap());
        let id = "money".to_string();
        let request = asked(
            &home,
            "s11",
            &id,
            vec![
                ("inv-1", call("c-1", "web_fetch", json!({ "url": "https://example.com/" }))),
                ("inv-1", response("c-1", "web_fetch", page("https://example.com/"))),
            ],
        )
        .await;
        let hints = home.registry.hints().reading(Arc::clone(home.engine.source()));
        let hint = hints.hint(&request).await.unwrap();
        assert_eq!(hint.first_step.as_deref(), Some("Money Matters read 1 web page (example.com)"));
    }
}

#[cfg(feature = "channels")]
mod chat_card {
    use super::*;
    use adk_kernel::channels::{ChannelSink, DecisionCard as ChannelCard};
    use adk_surface::channels::TelegramAdapter;

    /// The Telegram card of an agent's screen action is headed as the engine heads a chat card,
    /// "Snooze something in Coming up", never by the tool's name (spec 3.3 §5.3, §7.1).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_telegram_card_of_a_screen_action_takes_its_chat_heading() {
        let at = digest_at();
        let home = home(Setup::new(finance(at))).await;
        let request = ask(&home).await;
        let card = ChannelCard::from_request(&request, Some("http://127.0.0.1:8080/ui"));
        let api = super::fake_telegram::FakeTelegram::start().await;
        let adapter = |hinted: bool| {
            let mut adapter = TelegramAdapter::builder(super::fake_telegram::TOKEN)
                .api_base(api.url())
                .approvals(home.service.clone());
            if hinted {
                adapter = adapter.card_hints(Arc::new(home.registry.hints()));
            }
            adapter.build().unwrap()
        };
        adapter(true).send_decision("4217", &card).await.unwrap();
        let sent = api.last_card(&json!(4217)).unwrap();
        let text = sent.body["text"].as_str().unwrap();
        assert!(text.contains("\n<b>Snooze something in Coming up</b>\n"), "{text}");
        assert!(!text.contains("verb"), "{text}");
        assert!(sent.body.pointer("/reply_markup/inline_keyboard").is_some(), "Allow once · Deny");
        // Without the hints, the tool's name in plain words, as before.
        adapter(false).send_decision("4218", &card).await.unwrap();
        let plain = api.last_card(&json!(4218)).unwrap();
        assert!(plain.body["text"].as_str().unwrap().contains("<b>Verb snooze</b>"));
        // A hint that marks the call as spending money: chat offers no answer (§7.4 rule 4).
        struct Money;
        #[async_trait::async_trait]
        impl adk_surface::CardHints for Money {
            async fn hint(&self, _: &ApprovalRequest) -> Option<adk_surface::CardHint> {
                Some(adk_surface::CardHint::new().spends_money())
            }
        }
        let money = TelegramAdapter::builder(super::fake_telegram::TOKEN)
            .api_base(api.url())
            .approvals(home.service.clone())
            .card_hints(Arc::new(Money))
            .build()
            .unwrap();
        money.send_decision("4219", &card).await.unwrap();
        let paid = api.last_card(&json!(4219)).unwrap();
        assert!(paid.body.pointer("/reply_markup/inline_keyboard").is_none(), "{:?}", paid.body);
    }
}

#[cfg(feature = "voice")]
mod spoken {
    use super::*;
    use adk_realtime::scripted::{self, ScriptedReply};
    use adk_surface::voice::VoiceUpdate;
    use sha2::{Digest, Sha256};
    use verbs_support::voice::{hub, model_audio, says, speak, speech_started, until};

    /// Spec 3.3 §12 step 3, owner question 26: "Snooze news digest" waits out the engine's
    /// cancel window, then runs; its record carries the transcript's digest and never the
    /// words.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_spoken_snooze_runs_after_the_cancel_window() {
        let home =
            home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new())))).await;
        let hub = hub(&home, vec![says("Snooze news digest")]).await;
        let mut link = hub.open(&web(), "scripted", None).await.unwrap();
        speak(&link).await;
        let pending = until(&mut link, "the pending action", |u| {
            matches!(u, VoiceUpdate::PendingAction { .. })
        })
        .await;
        let to = home.at + Span::hours(1);
        let VoiceUpdate::PendingAction { heading, text, .. } = pending else { unreachable!() };
        assert_eq!(heading, format!("Move News digest to {}", time_text(to)));
        assert_eq!(text, "Snoozing in 1 s");
        assert_eq!(home.wake_of(DIGEST).await, Some(home.at), "nothing moves in the window");
        let done =
            until(&mut link, "the outcome", |u| matches!(u, VoiceUpdate::ActionOutcome { .. }))
                .await;
        assert!(matches!(done, VoiceUpdate::ActionOutcome { done: true, .. }), "{done:?}");
        assert_eq!(home.wake_of(DIGEST).await, Some(to));
        let record = home.verb_records().pop().unwrap();
        assert_eq!(record.ext["surface"], json!("voice"));
        assert_eq!(record.ext["origin"], json!({ "kind": "voice" }));
        let digest = format!("{:x}", Sha256::digest("Snooze news digest".as_bytes()));
        assert_eq!(record.ext["transcriptDigest"], json!(digest));
        let written = format!("{:?}", home.audit.records());
        assert!(!written.to_lowercase().contains("snooze news digest"), "the words are never kept");
    }

    /// The agenda of `home`'s kernel, holding each reschedule after it moved the wake until
    /// the test releases it: a snooze caught after its window, part way through.
    struct Held {
        inner: adk_surface::verbs::KernelAgenda,
        moved: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl adk_surface::verbs::AgendaAccess for Held {
        async fn items(
            &self,
            subject: &str,
            area: Option<&PartitionId>,
        ) -> Result<Vec<adk_kernel::AgendaItem>, VerbError> {
            self.inner.items(subject, area).await
        }

        async fn item(&self, id: &str) -> Result<Option<adk_kernel::AgendaItem>, VerbError> {
            self.inner.item(id).await
        }

        async fn started(&self, item: &adk_kernel::AgendaItem) -> Result<bool, VerbError> {
            self.inner.started(item).await
        }

        async fn reschedule(
            &self,
            id: &str,
            wake: WakeSet,
            by: &adk_core::principal::SubjectRef,
        ) -> Result<adk_kernel::AgendaItem, VerbError> {
            let moved = self.inner.reschedule(id, wake, by).await?;
            self.moved.notify_one();
            self.release.notified().await;
            Ok(moved)
        }
    }

    /// Spec 3.3 §5.6, §11: a cancel that arrives once the window has ended is too late — the
    /// snooze is already being taken, so it finishes, is recorded, and is reported as done,
    /// never as "Cancelled · nothing changed" over a wake that moved.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_cancel_after_the_window_does_not_stop_a_snooze_part_way() {
        use adk_surface::voice::{VoiceClientMessage, VoiceInput};
        let home =
            home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new())))).await;
        let (moved, release) =
            (Arc::new(tokio::sync::Notify::new()), Arc::new(tokio::sync::Notify::new()));
        let held = Held {
            inner: adk_surface::verbs::KernelAgenda::of(&home.kernel),
            moved: moved.clone(),
            release: release.clone(),
        };
        let registry = adk_surface::verbs::VerbRegistry::builder(APP, Arc::new(held))
            .audit(adk_core::audit_log::AuditTrail::new(Arc::new(home.audit.clone())))
            .utc_offset(offset())
            .build()
            .unwrap();
        let hub =
            verbs_support::voice::hub_with(&home, vec![says("snooze news digest")], registry).await;
        let mut link = hub.open(&web(), "scripted", None).await.unwrap();
        speak(&link).await;
        let pending = until(&mut link, "the pending action", |u| {
            matches!(u, VoiceUpdate::PendingAction { .. })
        })
        .await;
        let VoiceUpdate::PendingAction { action_id, .. } = pending else { unreachable!() };
        // The window ends and the snooze moves the wake; the cancel arrives just after.
        tokio::time::timeout(Duration::from_secs(10), moved.notified()).await.unwrap();
        link.input
            .send(VoiceInput::Message(VoiceClientMessage::CancelAction { action_id }))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        release.notify_one();
        let outcome =
            until(&mut link, "the outcome", |u| matches!(u, VoiceUpdate::ActionOutcome { .. }))
                .await;
        assert!(matches!(outcome, VoiceUpdate::ActionOutcome { done: true, .. }), "{outcome:?}");
        assert_eq!(home.wake_of(DIGEST).await, Some(home.at + Span::hours(1)));
        let done: Vec<_> = home
            .verb_records()
            .into_iter()
            .filter(|record| record.ext["outcome"] == "done")
            .collect();
        assert_eq!(done.len(), 1, "the snooze that ran is recorded");
    }

    /// "Snooze … tomorrow" for an item that ends before then says when it ends, as `/snooze`
    /// does; an ordinary sentence reads nothing of Coming up.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_spoken_snooze_says_why_not_and_other_words_read_no_agenda() {
        use adk_kernel::{AgendaItem, AgendaOrigin};
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Counted {
            inner: adk_surface::verbs::KernelAgenda,
            reads: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl adk_surface::verbs::AgendaAccess for Counted {
            async fn items(
                &self,
                subject: &str,
                area: Option<&PartitionId>,
            ) -> Result<Vec<AgendaItem>, VerbError> {
                self.reads.fetch_add(1, Ordering::SeqCst);
                self.inner.items(subject, area).await
            }
            async fn item(&self, id: &str) -> Result<Option<AgendaItem>, VerbError> {
                self.inner.item(id).await
            }
            async fn started(&self, item: &AgendaItem) -> Result<bool, VerbError> {
                self.inner.started(item).await
            }
            async fn reschedule(
                &self,
                id: &str,
                wake: WakeSet,
                by: &adk_core::principal::SubjectRef,
            ) -> Result<AgendaItem, VerbError> {
                self.inner.reschedule(id, wake, by).await
            }
        }
        let home =
            home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new())))).await;
        let ends = home.at + Span::hours(3);
        let mut walk = AgendaItem::new(
            "walk",
            adk_core::principal::SubjectRef::human(OWNER),
            "Evening walk",
            "why",
            AgendaOrigin::User,
            Utc::now(),
        )
        .with_partition(PartitionId::default())
        .with_state(AgendaState::Approved);
        walk.expires_at = Some(ends);
        home.kernel.agenda().store().put(walk).await.unwrap();
        home.move_to("walk", home.at).await;
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = Counted {
            inner: adk_surface::verbs::KernelAgenda::of(&home.kernel),
            reads: reads.clone(),
        };
        let registry = adk_surface::verbs::VerbRegistry::builder(APP, Arc::new(counted))
            .utc_offset(offset())
            .build()
            .unwrap();
        let hub = verbs_support::voice::hub_with(
            &home,
            vec![says("what's on today"), says("snooze evening walk tomorrow")],
            registry,
        )
        .await;
        let mut link = hub.open(&web(), "scripted", None).await.unwrap();
        speak(&link).await;
        until(
            &mut link,
            "the sentence",
            |u| matches!(u, VoiceUpdate::Heard { text } if text == "what's on today"),
        )
        .await;
        speak(&link).await;
        let outcome =
            until(&mut link, "the outcome", |u| matches!(u, VoiceUpdate::ActionOutcome { .. }))
                .await;
        let VoiceUpdate::ActionOutcome { text, done, .. } = outcome else { unreachable!() };
        assert!(!done);
        assert_eq!(
            text,
            format!("Can't snooze: It ends at {}, so it can't move past then", time_text(ends))
        );
        assert_eq!(reads.load(Ordering::SeqCst), 1, "only the snooze read Coming up");
    }

    /// "Cancel" in the window withdraws it; the model's own audio never takes an action.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancel_and_echo_change_nothing() {
        let home =
            home(Setup::new(Arc::new(ScriptedLlm::new("s", Vec::<ScriptedResponse>::new())))).await;
        let hub = hub(
            &home,
            vec![
                says("snooze news digest"),
                says("cancel"),
                ScriptedReply::new(vec![
                    model_audio(1_000),
                    speech_started(),
                    scripted::input_transcript("snooze news digest"),
                ])
                .on_audio(),
            ],
        )
        .await;
        let mut link = hub.open(&web(), "scripted", None).await.unwrap();
        speak(&link).await;
        until(&mut link, "the pending action", |u| matches!(u, VoiceUpdate::PendingAction { .. }))
            .await;
        speak(&link).await;
        let cancelled =
            until(&mut link, "the cancel", |u| matches!(u, VoiceUpdate::ActionOutcome { .. }))
                .await;
        assert!(
            matches!(&cancelled, VoiceUpdate::ActionOutcome { done: false, text, .. } if text == "Cancelled · nothing changed"),
            "{cancelled:?}"
        );
        // The model says it, and the microphone hears it back.
        speak(&link).await;
        until(
            &mut link,
            "the echo",
            |u| matches!(u, VoiceUpdate::Heard { text } if text == "snooze news digest"),
        )
        .await;
        tokio::time::sleep(WINDOW + Duration::from_millis(500)).await;
        assert_eq!(home.wake_of(DIGEST).await, Some(home.at));
        assert!(home.verb_records().is_empty());
    }
}
