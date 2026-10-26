//! Voice and video through the engine (spec 3.3 §5.6, §7.2, §7.4 rules 4 and 6, §11 S-I2,
//! S-I4, S-I8, S-I10): spoken answers recorded only with their transcript's digest, bound to
//! the prompted decision, resumed on the voice conversation that held the call.
//!
//! The realtime provider is `ScriptedRealtimeModel`: the person's speech and the model's turns
//! are scripted. The harness ("Chief of staff", `Pack::Voice`, Ask first), its governed tools,
//! the approval inbox, the session store, the engine and the audit trail are real. On
//! `origin/main` none of this exists: `execute` takes a spoken answer without a transcript,
//! and a decision on a voice-held call resumes it on the text runner.
//! The fixture is `voice_common`.

#![cfg(feature = "voice")]

mod voice_common;

use std::time::Duration;

use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope, ApprovalState,
    ApproverSelector, SubjectRef, ToolConfirmationRequest,
};
use adk_realtime::scripted::{self, ScriptedReply};
use adk_session::CreateRequest;
use adk_surface::voice::{VoiceClientMessage, VoiceInput, VoiceUpdate};
use adk_surface::{Answer, Caller, Command, CommandOutcome, Origin, SurfaceKind, words};
use serde_json::json;
use voice_common::*;

/// The engine's own guarantees, without a realtime session (spec 3.3 §5.6, §11).
mod engine {
    use super::*;

    async fn hold(voice: &Voice, tool: &str, risk: ApprovalRisk) -> ApprovalRequest {
        voice
            .sessions
            .create(CreateRequest {
                app_name: APP.into(),
                user_id: OWNER.into(),
                session_id: Some(format!("s-{tool}")),
                state: Default::default(),
            })
            .await
            .ok();
        let call = ToolConfirmationRequest {
            tool_name: tool.into(),
            function_call_id: Some(format!("call-{tool}")),
            args: json!({ "to": "[CLIENT]", "body": "x".repeat(if tool == "write_file" { 3_000 } else { 10 }) }),
            origin: None,
            acting: None,
        };
        let request = ApprovalRequest::new(
            format!("apr-{tool}"),
            ApprovalScope::new(APP, OWNER, format!("s-{tool}")),
            &call,
            ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
        )
        .with_risk(risk);
        voice.service.submit(request).await.unwrap()
    }

    async fn spoken(
        voice: &Voice,
        request: &ApprovalRequest,
        answer: Answer,
        said: &str,
    ) -> adk_surface::Result<adk_surface::voice::SpokenAnswer> {
        voice.engine.decide_spoken(&speaker(), &request.id, request.version, answer, said).await
    }

    /// Every record of a spoken answer carries the transcript's SHA-256, surface voice and
    /// origin voice, and none holds the words.
    #[tokio::test]
    async fn spoken_records_carry_the_digest_and_never_the_words() {
        let voice = voice(Vec::new()).await;
        let decided = hold(&voice, "send_email", ApprovalRisk::Medium).await;
        let cancelled = hold(&voice, "send_sms", ApprovalRisk::Low).await;
        let refused = hold(&voice, "publish_release", ApprovalRisk::High).await;

        let answer =
            spoken(&voice, &decided, Answer::AllowOnce, "Allow once, please.").await.unwrap();
        assert!(matches!(answer.outcome, CommandOutcome::Pending { .. }));
        let settled = answer.settled.expect("a pending answer settles").await.unwrap().unwrap();
        assert!(matches!(settled, CommandOutcome::Answered { .. }), "{settled:?}");
        assert_eq!(state(&voice, &decided.id).await, ApprovalState::Approved);

        let answer = spoken(&voice, &cancelled, Answer::AllowOnce, "allow once").await.unwrap();
        voice
            .engine
            .execute(
                &Caller::human(OWNER, SurfaceKind::Voice, Origin::Pointer).with_groups(["owner"]),
                Command::CancelAnswer { decision_id: cancelled.id.clone() },
            )
            .await
            .unwrap();
        assert!(answer.settled.unwrap().await.is_err(), "a cancelled answer never settles");
        assert_eq!(state(&voice, &cancelled.id).await, ApprovalState::Pending);

        let error = spoken(&voice, &refused, Answer::AllowOnce, "allow once").await.unwrap_err();
        assert_eq!(error.code(), "surface.answer_not_allowed");

        let records = decide_records(&voice.audit);
        let outcomes: Vec<(&str, &str)> = records
            .iter()
            .map(|record| {
                (
                    record.ext["approvalId"].as_str().unwrap(),
                    record.ext["outcome"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            outcomes,
            [
                (decided.id.as_str(), "pending"),
                (decided.id.as_str(), "decided"),
                (cancelled.id.as_str(), "pending"),
                (cancelled.id.as_str(), "cancelled"),
                (refused.id.as_str(), "refused"),
            ]
        );
        for record in &records {
            let said = match record.ext["approvalId"].as_str() {
                Some(id) if id == decided.id => "Allow once, please.",
                _ => "allow once",
            };
            assert_eq!(record.ext["transcriptDigest"], json!(digest(said)), "{record:?}");
            let text = format!("{record:?}").to_lowercase();
            assert!(!text.contains("allow once"), "a record holds the words: {text}");
        }
        for record in records.iter().filter(|r| r.ext["outcome"] != "cancelled") {
            assert_eq!(record.ext["surface"], "voice");
            assert_eq!(record.ext["origin"], json!({ "kind": "voice" }));
        }
    }

    #[tokio::test]
    async fn a_spoken_answer_needs_a_spoken_caller_and_execute_takes_none() {
        let voice = voice(Vec::new()).await;
        let request = hold(&voice, "send_email", ApprovalRisk::Medium).await;
        let pointer =
            Caller::human(OWNER, SurfaceKind::Voice, Origin::Pointer).with_groups(["owner"]);
        let error = voice
            .engine
            .decide_spoken(&pointer, &request.id, request.version, Answer::Deny, "deny")
            .await
            .unwrap_err();
        assert_eq!(error.code(), "surface.invalid");
        for caller in [
            speaker(),
            Caller::human(OWNER, SurfaceKind::Web, Origin::Voice).with_groups(["owner"]),
        ] {
            let error = voice
                .engine
                .execute(
                    &caller,
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
                .unwrap_err();
            assert_eq!(error.code(), "surface.invalid", "{caller:?}");
        }
        assert_eq!(state(&voice, &request.id).await, ApprovalState::Pending);
        assert!(decide_records(&voice.audit).is_empty());
    }

    #[tokio::test]
    async fn a_truncated_call_is_decided_on_screen() {
        let voice = voice(Vec::new()).await;
        let request = hold(&voice, "write_file", ApprovalRisk::Low).await;
        let error = spoken(&voice, &request, Answer::AllowOnce, "allow once").await.unwrap_err();
        assert_eq!(error.code(), "surface.call_not_expanded");
        let record = decide_records(&voice.audit).pop().unwrap();
        assert_eq!(record.ext["outcome"], "refused");
        assert_eq!(record.ext["transcriptDigest"], json!(digest("allow once")));
    }

    /// A stop that lands inside the window — here, or anywhere else — means nothing runs.
    #[tokio::test]
    async fn stop_all_inside_the_window_cancels_the_answer() {
        let voice = voice(Vec::new()).await;
        let request = hold(&voice, "send_email", ApprovalRisk::Medium).await;
        let answer = spoken(&voice, &request, Answer::AllowOnce, "allow once").await.unwrap();
        voice.engine.execute(&web(), Command::StopAll { reason: None }).await.unwrap();
        let settled = answer.settled.unwrap().await;
        assert!(!matches!(settled, Ok(Ok(CommandOutcome::Answered { .. }))), "{settled:?}");
        tokio::time::sleep(WINDOW).await;
        assert_eq!(state(&voice, &request.id).await, ApprovalState::Pending);
        assert_eq!(
            stopped_answers(&voice),
            [json!(request.id)],
            "the cancelled answer is on record"
        );
    }

    #[tokio::test]
    async fn a_spoken_deny_is_recorded_at_once() {
        let voice = voice(Vec::new()).await;
        let request = hold(&voice, "send_email", ApprovalRisk::Medium).await;
        let answer = spoken(&voice, &request, Answer::Deny, "deny").await.unwrap();
        assert!(matches!(answer.outcome, CommandOutcome::Answered { .. }));
        assert!(answer.settled.is_none());
        assert_eq!(state(&voice, &request.id).await, ApprovalState::Rejected);
        let card = voice.engine.decision(&web(), &request.id).await.unwrap();
        assert!(card.decided.unwrap().text.contains(words::BY_VOICE));
    }
}

/// Conversations through the hub, on a scripted realtime model; the commands run in `bash`.
#[cfg(unix)]
mod conversation {
    use std::sync::Arc;

    use adk_core::{
        ApprovalCall, ApprovalDelegation, ApprovalService, ApprovalStore, ApprovalTick, InboxPage,
        InboxQuery,
    };
    use chrono::{DateTime, Utc};

    use super::*;

    /// How long [`SlowDecide`] takes to record a decision.
    const SLOW_DECIDE: Duration = Duration::from_millis(800);

    /// An approval service whose decisions take [`SLOW_DECIDE`] to record.
    #[derive(Debug)]
    struct SlowDecide(Arc<dyn ApprovalService>);

    #[async_trait::async_trait]
    impl ApprovalService for SlowDecide {
        async fn submit(&self, request: ApprovalRequest) -> adk_core::Result<ApprovalRequest> {
            self.0.submit(request).await
        }

        async fn submit_call(
            &self,
            call: ApprovalCall,
        ) -> adk_core::Result<Option<ApprovalRequest>> {
            self.0.submit_call(call).await
        }

        async fn get(&self, id: &str) -> adk_core::Result<Option<ApprovalRequest>> {
            self.0.get(id).await
        }

        async fn inbox(&self, query: InboxQuery) -> adk_core::Result<InboxPage> {
            self.0.inbox(query).await
        }

        async fn decide(
            &self,
            id: &str,
            expected_version: u64,
            decision: ApprovalDecision,
        ) -> adk_core::Result<ApprovalRequest> {
            tokio::time::sleep(SLOW_DECIDE).await;
            self.0.decide(id, expected_version, decision).await
        }

        async fn delegate(
            &self,
            delegation: ApprovalDelegation,
        ) -> adk_core::Result<ApprovalDelegation> {
            self.0.delegate(delegation).await
        }

        async fn revoke_delegation(&self, id: &str) -> adk_core::Result<bool> {
            self.0.revoke_delegation(id).await
        }

        async fn delegations_to(
            &self,
            to: &SubjectRef,
        ) -> adk_core::Result<Vec<ApprovalDelegation>> {
            self.0.delegations_to(to).await
        }

        async fn cancel(
            &self,
            id: &str,
            reason: Option<String>,
        ) -> adk_core::Result<ApprovalRequest> {
            self.0.cancel(id, reason).await
        }

        async fn withdraw(
            &self,
            id: &str,
            reason: Option<String>,
        ) -> adk_core::Result<ApprovalRequest> {
            self.0.withdraw(id, reason).await
        }

        async fn tick(&self, now: DateTime<Utc>) -> adk_core::Result<ApprovalTick> {
            self.0.tick(now).await
        }

        fn approval_store(&self) -> Option<Arc<dyn ApprovalStore>> {
            self.0.approval_store()
        }
    }

    /// Another agent's medium-risk call, waiting in its own session.
    async fn other_session_decision(voice: &Voice) -> ApprovalRequest {
        voice
            .sessions
            .create(CreateRequest {
                app_name: APP.into(),
                user_id: OWNER.into(),
                session_id: Some("finance".into()),
                state: Default::default(),
            })
            .await
            .unwrap();
        let call = ToolConfirmationRequest {
            tool_name: "send_email".into(),
            function_call_id: Some("call-f".into()),
            args: json!({ "to": "[CLIENT]" }),
            origin: None,
            acting: None,
        };
        voice
            .service
            .submit(
                ApprovalRequest::new(
                    "apr-finance",
                    ApprovalScope::new(APP, OWNER, "finance"),
                    &call,
                    ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
                )
                .with_risk(ApprovalRisk::Medium),
            )
            .await
            .unwrap()
    }

    /// The agent's own call: "allow once" runs it exactly once, after the window, and every
    /// screen reads "You said yes by voice".
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn allow_once_runs_the_call_once_after_the_window() {
        let mut script = asks_for(SEND);
        script.push(says("allow once"));
        script.push(ScriptedReply::new(scripted::spoken("Sent.")).on_tool_output());
        let voice = voice(script).await;
        let mut link = voice.hub.open(&web(), "scripted", None).await.unwrap();
        speak(&link).await;
        let (id, _) = prompted(&mut link).await;
        tokio::time::sleep(DWELL).await;
        speak(&link).await;
        until(
            &mut link,
            "the heard answer",
            |u| matches!(u, VoiceUpdate::Heard { text } if text == "allow once"),
        )
        .await;
        let pending =
            until(&mut link, "the pending answer", |u| matches!(u, VoiceUpdate::Pending { .. }))
                .await;
        assert!(
            matches!(&pending, VoiceUpdate::Pending { decision_id, text, .. } if *decision_id == id && text.starts_with("Allowing once in"))
        );
        assert_eq!(voice.sent(), 0, "nothing runs inside the window");
        let outcome =
            until(&mut link, "the outcome", |u| matches!(u, VoiceUpdate::Outcome { .. })).await;
        assert_eq!(
            outcome,
            VoiceUpdate::Outcome { decision_id: id.clone(), decided: true, text: None }
        );
        eventually("the call ran", async || (voice.sent() == 1).then_some(())).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(voice.sent(), 1, "it ran exactly once");
        let card = voice.engine.decision(&web(), &id).await.unwrap();
        assert!(card.decided.unwrap().text.starts_with("You said yes by voice at "));
        drop(link);
    }

    /// Cancel by voice, End, and Stop all inside the window leave the call waiting.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancel_end_and_stop_all_leave_the_call_waiting() {
        for how in ["cancel", "end", "stop all"] {
            let mut script = asks_for(SEND);
            script.push(says("allow once"));
            if how != "end" {
                script.push(says(how));
            }
            let voice = voice(script).await;
            let mut link = voice.hub.open(&web(), "scripted", None).await.unwrap();
            speak(&link).await;
            let (id, _) = prompted(&mut link).await;
            tokio::time::sleep(DWELL).await;
            speak(&link).await;
            until(&mut link, "the pending answer", |u| matches!(u, VoiceUpdate::Pending { .. }))
                .await;
            match how {
                "end" => {
                    link.input.send(VoiceInput::Message(VoiceClientMessage::End)).await.unwrap()
                }
                _ => speak(&link).await,
            }
            let outcome =
                until(&mut link, "the outcome", |u| matches!(u, VoiceUpdate::Outcome { .. })).await;
            assert!(
                matches!(outcome, VoiceUpdate::Outcome { decided: false, .. }),
                "{how}: {outcome:?}"
            );
            tokio::time::sleep(WINDOW + Duration::from_millis(200)).await;
            assert_eq!(voice.sent(), 0, "{how}");
            assert_eq!(state(&voice, &id).await, ApprovalState::Pending, "{how}");
            let cancelled = decide_records(&voice.audit)
                .into_iter()
                .filter(|record| record.ext["outcome"] == "cancelled")
                .collect::<Vec<_>>();
            if how == "stop all" {
                // Stop all withdraws it, on its own record (spec 3.3 S-I10).
                assert!(cancelled.is_empty(), "{how}");
                assert_eq!(stopped_answers(&voice), [json!(id)], "{how}");
            } else {
                assert_eq!(cancelled.len(), 1, "{how}");
                assert_eq!(
                    cancelled[0].ext["transcriptDigest"],
                    json!(digest("allow once")),
                    "{how}"
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn deny_rejects() {
        let mut script = asks_for(SEND);
        script.push(says("Deny."));
        let voice = voice(script).await;
        let mut link = voice.hub.open(&web(), "scripted", None).await.unwrap();
        speak(&link).await;
        let (id, _) = prompted(&mut link).await;
        tokio::time::sleep(DWELL).await;
        speak(&link).await;
        let outcome =
            until(&mut link, "the outcome", |u| matches!(u, VoiceUpdate::Outcome { .. })).await;
        assert_eq!(
            outcome,
            VoiceUpdate::Outcome { decision_id: id.clone(), decided: true, text: None }
        );
        assert_eq!(state(&voice, &id).await, ApprovalState::Rejected);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(voice.sent(), 0);
    }

    /// High risk: "allow once" decides nothing; a paired local terminal approves, and the
    /// call runs once, on the voice conversation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn high_risk_is_approved_on_a_screen_not_by_voice() {
        let mut script = asks_for(PUBLISH);
        script.push(says("allow once"));
        script.push(ScriptedReply::new(scripted::spoken("Published.")).on_tool_output());
        let voice = voice(script).await;
        let mut link = voice.hub.open(&web(), "scripted", None).await.unwrap();
        speak(&link).await;
        let screen = until(
            &mut link,
            "needs a screen",
            |u| matches!(u, VoiceUpdate::NeedsScreen { decision_ids } if !decision_ids.is_empty()),
        )
        .await;
        let VoiceUpdate::NeedsScreen { decision_ids } = screen else { unreachable!() };
        let id = decision_ids[0].clone();
        tokio::time::sleep(DWELL).await;
        speak(&link).await;
        until(&mut link, "the heard answer", |u| matches!(u, VoiceUpdate::Heard { .. })).await;
        tokio::time::sleep(WINDOW + Duration::from_millis(200)).await;
        assert_eq!(voice.published(), 0);
        assert_eq!(state(&voice, &id).await, ApprovalState::Pending);
        assert!(
            decide_records(&voice.audit).iter().all(|record| record.ext["outcome"] != "pending"),
            "nothing is pending"
        );

        let terminal = Caller::human(OWNER, SurfaceKind::Terminal, Origin::Keyboard)
            .with_groups(["owner"])
            .with_paired_local();
        let request = voice.service.get(&id).await.unwrap().unwrap();
        voice
            .engine
            .execute(
                &terminal,
                Command::Decide {
                    decision_id: id.clone(),
                    version: request.version,
                    answer: Answer::AllowOnce,
                    edited_args: None,
                    reason: None,
                    expanded: true,
                },
            )
            .await
            .unwrap();
        eventually("the release ran", async || (voice.published() == 1).then_some(())).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(voice.published(), 1, "once, never on the text runner too");
        let card = voice.engine.decision(&web(), &id).await.unwrap();
        assert!(card.decided.unwrap().text.contains(words::IN_THE_TERMINAL));
    }

    /// The model's own words, "yes", speech over the model's audio, and an answer to a card
    /// that changed while the person spoke decide nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn nothing_but_the_persons_own_answer_to_the_prompted_card_decides() {
        let mut script = asks_for(SEND);
        // The model says "allow once"; the person says "yes"; then "allow once" over the
        // model's audio.
        script.push(
            ScriptedReply::new(
                [
                    scripted::spoken("Say allow once or deny."),
                    vec![
                        scripted::input_transcript("yes"),
                        model_audio(1_000),
                        speech_started(),
                        scripted::input_transcript("allow once"),
                    ],
                ]
                .concat(),
            )
            .on_audio(),
        );
        let voice = voice(script).await;
        let mut link = voice.hub.open(&web(), "scripted", None).await.unwrap();
        speak(&link).await;
        let (id, _) = prompted(&mut link).await;
        tokio::time::sleep(DWELL).await;
        speak(&link).await;
        until(
            &mut link,
            "the echo",
            |u| matches!(u, VoiceUpdate::Heard { text } if text == "allow once"),
        )
        .await;
        tokio::time::sleep(WINDOW + Duration::from_millis(200)).await;
        assert_eq!(state(&voice, &id).await, ApprovalState::Pending);
        assert!(decide_records(&voice.audit).is_empty(), "nothing was answered");
        assert_eq!(voice.sent(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_card_that_changes_while_the_person_speaks_is_not_decided() {
        let voice = voice(vec![
            ScriptedReply::new(vec![speech_started()]).on_audio(),
            ScriptedReply::new(vec![scripted::input_transcript("allow once")]).on_audio(),
        ])
        .await;
        // Another agent's call, needing two approvals: one arrives while the person speaks.
        voice
            .sessions
            .create(CreateRequest {
                app_name: APP.into(),
                user_id: OWNER.into(),
                session_id: Some("finance".into()),
                state: Default::default(),
            })
            .await
            .unwrap();
        let call = ToolConfirmationRequest {
            tool_name: "send_email".into(),
            function_call_id: Some("call-f".into()),
            args: json!({ "to": "[CLIENT]" }),
            origin: None,
            acting: None,
        };
        let request = voice
            .service
            .submit(
                ApprovalRequest::new(
                    "apr-finance",
                    ApprovalScope::new(APP, OWNER, "finance"),
                    &call,
                    ApprovalPolicy::new([ApproverSelector::group("owner")])
                        .allow_self_approval()
                        .with_quorum(2),
                )
                .with_risk(ApprovalRisk::Medium),
            )
            .await
            .unwrap();
        let mut link = voice.hub.open(&web(), "scripted", None).await.unwrap();
        let (id, version) = prompted(&mut link).await;
        assert_eq!((id.as_str(), version), (request.id.as_str(), request.version));
        tokio::time::sleep(DWELL).await;
        speak(&link).await;
        voice
            .service
            .decide(
                &id,
                version,
                ApprovalDecision::approve(SubjectRef::human("bo")).with_groups(["owner"]),
            )
            .await
            .unwrap();
        until(
            &mut link,
            "the new version",
            |u| matches!(u, VoiceUpdate::Prompted { version: Some(v), .. } if *v > version),
        )
        .await;
        speak(&link).await;
        let outcome =
            until(&mut link, "the outcome", |u| matches!(u, VoiceUpdate::Outcome { .. })).await;
        assert_eq!(
            outcome,
            VoiceUpdate::Outcome {
                decision_id: id.clone(),
                decided: false,
                text: Some(words::CHANGED_WHILE_YOU_SPOKE.into())
            }
        );
        assert_eq!(state(&voice, &id).await, ApprovalState::Pending);
        // Another agent's card is shown, never told to the model.
        assert!(voice.model.log().texts().is_empty(), "{:?}", voice.model.log().texts());
    }

    /// A decision made straight in the inbox resumes the voice-held call within a tick, once,
    /// and the hold and the resumed call are on the record (harness/voice.md, "Audits").
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_decision_in_the_inbox_resumes_the_voice_held_call_once() {
        let mut script = asks_for(SEND);
        script.push(ScriptedReply::new(scripted::spoken("Sent.")).on_tool_output());
        let voice = voice(script).await;
        let mut link = voice.hub.open(&web(), "scripted", None).await.unwrap();
        speak(&link).await;
        let (id, version) = prompted(&mut link).await;
        voice
            .service
            .decide(
                &id,
                version,
                ApprovalDecision::approve(SubjectRef::human(OWNER)).with_groups(["owner"]),
            )
            .await
            .unwrap();
        eventually("the call ran", async || (voice.sent() == 1).then_some(())).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(voice.sent(), 1);
        let records = |action: &str| {
            voice
                .audit
                .records()
                .into_iter()
                .filter(|record| record.action == action)
                .map(|record| {
                    (record.ext["functionCallId"].clone(), record.ext.get("executed").cloned())
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(records(adk_harness::VOICE_AUDIT_HELD), [(json!("call-1"), None)]);
        assert_eq!(
            records(adk_harness::VOICE_AUDIT_RESUMED),
            [(json!("call-1"), Some(json!(true)))]
        );
    }

    /// Stop all reaches a live conversation: a call it held does not run when it is approved
    /// after the stop, and the conversation ends; one opened while every agent is stopped
    /// ends at once (spec 3.3 S-I10, §12 phase 5).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stop_all_ends_the_conversation_and_refuses_its_calls() {
        let mut script = asks_for(SEND);
        script.push(ScriptedReply::new(scripted::spoken("Sent.")).on_tool_output());
        let voice = voice(script).await;
        let mut link = voice.hub.open(&web(), "scripted", None).await.unwrap();
        speak(&link).await;
        let (id, version) = prompted(&mut link).await;
        voice.engine.execute(&web(), Command::StopAll { reason: None }).await.unwrap();
        let terminal = Caller::human(OWNER, SurfaceKind::Terminal, Origin::Keyboard)
            .with_groups(["owner"])
            .with_paired_local();
        voice
            .engine
            .execute(
                &terminal,
                Command::Decide {
                    decision_id: id.clone(),
                    version,
                    answer: Answer::AllowOnce,
                    edited_args: None,
                    reason: None,
                    expanded: true,
                },
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(voice.sent(), 0, "nothing runs while every agent is stopped");
        let stopped = VoiceUpdate::Error {
            code: "surface.stopped".into(),
            message: words::ALL_AGENTS_STOPPED.into(),
        };
        until(&mut link, "the stop", |u| *u == stopped).await;
        until(&mut link, "the end", |u| matches!(u, VoiceUpdate::Ended)).await;

        let mut again = voice.hub.open(&web(), "scripted", None).await.unwrap();
        let first = until(&mut again, "the first update", |_| true).await;
        assert_eq!(first, stopped, "no conversation runs while every agent is stopped");
        until(&mut again, "the end", |u| matches!(u, VoiceUpdate::Ended)).await;
        assert_eq!(voice.sent(), 0);
    }

    /// A tool the harness's model has beside the harness toolset — here one added with
    /// `.tool()` — is a tool of the voice conversation too (spec 3.3 §5.6).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_conversation_has_every_tool_the_harness_model_has() {
        let voice = voice(vec![
            ScriptedReply::new(
                [
                    vec![speech_started(), scripted::input_transcript("what's on today?")],
                    scripted::calls("call-c", "calendar", json!({})),
                ]
                .concat(),
            )
            .on_audio(),
        ])
        .await;
        let mut link = voice.hub.open(&web(), "scripted", None).await.unwrap();
        until(&mut link, "the conversation", |u| matches!(u, VoiceUpdate::Ready { .. })).await;
        let declared: Vec<String> = voice.model.log().configs()[0]
            .tools
            .iter()
            .flatten()
            .map(|tool| tool.name.clone())
            .collect();
        assert!(declared.contains(&"calendar".to_string()), "{declared:?}");
        assert!(declared.contains(&"bash".to_string()), "{declared:?}");
        speak(&link).await;
        let output = eventually("the calendar's answer", async || {
            voice.model.log().tool_outputs().into_iter().find(|output| output.call_id == "call-c")
        })
        .await;
        assert_eq!(output.output, json!({ "events": ["Standup at 09:30"] }));
    }

    /// An approval recorded while the conversation is ending still runs the call: no later
    /// run on the voice session would (spec 3.3 S-I3).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_approval_recorded_while_the_conversation_ends_still_runs() {
        let mut script = asks_for(SEND);
        script.push(says("allow once"));
        script.push(ScriptedReply::new(scripted::spoken("Sent.")).on_tool_output());
        // A store whose write takes a while, as a database's can.
        let voice = voice_built(
            script,
            |model| Arc::new(model),
            |service| Arc::new(SlowDecide(service)) as Arc<dyn ApprovalService>,
        )
        .await;
        let mut link = voice.hub.open(&web(), "scripted", None).await.unwrap();
        speak(&link).await;
        let (id, _) = prompted(&mut link).await;
        tokio::time::sleep(DWELL).await;
        speak(&link).await;
        until(&mut link, "the pending answer", |u| matches!(u, VoiceUpdate::Pending { .. })).await;
        // The window ends and the approval is being written when the person presses End.
        tokio::time::sleep(WINDOW + SLOW_DECIDE / 2).await;
        link.input.send(VoiceInput::Message(VoiceClientMessage::End)).await.unwrap();
        let outcome =
            until(&mut link, "the outcome", |u| matches!(u, VoiceUpdate::Outcome { .. })).await;
        assert_eq!(
            outcome,
            VoiceUpdate::Outcome { decision_id: id.clone(), decided: true, text: None }
        );
        until(&mut link, "the end", |u| matches!(u, VoiceUpdate::Ended)).await;
        assert_eq!(state(&voice, &id).await, ApprovalState::Approved);
        eventually("the approved call ran", async || (voice.sent() == 1).then_some(())).await;
    }

    /// Once the transport says it flushed the audio a barge-in interrupted, speech counts
    /// again; until it does, speech over that audio is still taken for its echo (spec 3.3
    /// §5.6).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn after_a_flushed_barge_in_a_spoken_deny_counts() {
        let mut script = asks_for(SEND);
        // Fifteen seconds of the model's audio, sent at once, and the person talks over it.
        script.push(
            ScriptedReply::new(vec![
                model_audio(15_000),
                speech_started(),
                scripted::input_transcript("wait"),
            ])
            .on_audio(),
        );
        script.push(says("deny"));
        script.push(says("deny"));
        let voice = voice(script).await;
        let mut link = voice.hub.open(&web(), "scripted", None).await.unwrap();
        speak(&link).await;
        let (id, _) = prompted(&mut link).await;
        tokio::time::sleep(DWELL).await;
        speak(&link).await;
        until(&mut link, "the barge-in", |u| matches!(u, VoiceUpdate::BargeIn)).await;
        until(&mut link, "the interruption", |u| matches!(u, VoiceUpdate::Heard { .. })).await;

        // Not flushed yet: the model's audio may still play into the microphone.
        speak(&link).await;
        until(
            &mut link,
            "the echo",
            |u| matches!(u, VoiceUpdate::Heard { text } if text == "deny"),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(state(&voice, &id).await, ApprovalState::Pending);

        link.input.send(VoiceInput::Message(VoiceClientMessage::PlaybackEnded)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        speak(&link).await;
        let outcome =
            until(&mut link, "the outcome", |u| matches!(u, VoiceUpdate::Outcome { .. })).await;
        assert_eq!(
            outcome,
            VoiceUpdate::Outcome { decision_id: id.clone(), decided: true, text: None }
        );
        assert_eq!(state(&voice, &id).await, ApprovalState::Rejected);
    }

    /// Speech that starts before the prompt has been on screen for a second decides nothing
    /// (spec 3.3 §5.6).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn speech_before_the_prompt_dwell_decides_nothing() {
        let mut script = asks_for(SEND);
        script.push(says("deny"));
        script.push(says("deny"));
        let voice = voice(script).await;
        let mut link = voice.hub.open(&web(), "scripted", None).await.unwrap();
        speak(&link).await;
        let (id, _) = prompted(&mut link).await;
        speak(&link).await;
        until(&mut link, "the early answer", |u| matches!(u, VoiceUpdate::Heard { .. })).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(state(&voice, &id).await, ApprovalState::Pending);
        assert!(decide_records(&voice.audit).is_empty(), "nothing was answered");

        tokio::time::sleep(DWELL).await;
        speak(&link).await;
        until(&mut link, "the outcome", |u| matches!(u, VoiceUpdate::Outcome { .. })).await;
        assert_eq!(state(&voice, &id).await, ApprovalState::Rejected);
    }

    /// A transcript that arrives after the next utterance started answers the decision
    /// prompted when its own speech started (spec 3.3 §5.6): here that decision was answered
    /// elsewhere meanwhile, so nothing is decided.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_late_transcript_is_bound_to_its_own_utterance() {
        let mut script = asks_for(SEND);
        // The person starts speaking; its transcript comes only after the next utterance began.
        script.push(ScriptedReply::new(vec![speech_started()]).on_audio());
        script.push(
            ScriptedReply::new(vec![speech_started(), scripted::input_transcript("allow once")])
                .on_audio(),
        );
        let voice = voice(script).await;
        let finance = other_session_decision(&voice).await;
        let mut link = voice.hub.open(&web(), "scripted", None).await.unwrap();
        speak(&link).await;
        let own = until(&mut link, "the conversation's own call", |u| {
            matches!(u, VoiceUpdate::Prompted { decision_id: Some(id), .. } if *id != finance.id)
        })
        .await;
        let VoiceUpdate::Prompted { decision_id: Some(own), version: Some(version) } = own else {
            unreachable!()
        };
        tokio::time::sleep(DWELL).await;
        speak(&link).await;
        // Answered elsewhere while the person speaks: the next decision is prompted.
        voice
            .service
            .decide(
                &own,
                version,
                ApprovalDecision::reject(SubjectRef::human(OWNER), "no").with_groups(["owner"]),
            )
            .await
            .unwrap();
        until(&mut link, "the next decision", |u| {
            matches!(u, VoiceUpdate::Prompted { decision_id: Some(id), .. } if *id == finance.id)
        })
        .await;
        tokio::time::sleep(DWELL).await;
        speak(&link).await;
        let outcome =
            until(&mut link, "the outcome", |u| matches!(u, VoiceUpdate::Outcome { .. })).await;
        assert_eq!(
            outcome,
            VoiceUpdate::Outcome {
                decision_id: own.clone(),
                decided: false,
                text: Some(words::CHANGED_WHILE_YOU_SPOKE.into())
            }
        );
        tokio::time::sleep(WINDOW + Duration::from_millis(200)).await;
        assert_eq!(state(&voice, &finance.id).await, ApprovalState::Pending);
    }

    /// A call shown cut short is decided on screen: voice may deny it, never allow it, and it
    /// is listed with the decisions voice cannot approve (spec 3.3 §5.6, §7.2).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_call_shown_cut_short_is_listed_for_the_screen_and_may_be_denied() {
        let long = format!("printf x >> sent.txt # {}", "a".repeat(adk_surface::CALL_TEXT_LIMIT));
        let mut script = asks_for(&long);
        script.push(says("deny"));
        let voice = voice(script).await;
        let mut link = voice.hub.open(&web(), "scripted", None).await.unwrap();
        speak(&link).await;
        let (id, _) = prompted(&mut link).await;
        let card = voice.engine.decision(&web(), &id).await.unwrap();
        assert!(card.call.truncated);
        until(&mut link, "needs a screen", |u| {
            matches!(u, VoiceUpdate::NeedsScreen { decision_ids } if decision_ids.contains(&id))
        })
        .await;
        tokio::time::sleep(DWELL).await;
        speak(&link).await;
        until(&mut link, "the outcome", |u| matches!(u, VoiceUpdate::Outcome { .. })).await;
        assert_eq!(state(&voice, &id).await, ApprovalState::Rejected);
        assert_eq!(voice.sent(), 0);
    }

    /// A conversation for a screen that shows only its own session — the terminal's thread —
    /// prompts only its own decisions, never one that screen does not show (spec 3.3 §7.1).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_conversation_in_a_thread_answers_only_its_own_decisions() {
        let voice = voice(asks_for(SEND)).await;
        let finance = other_session_decision(&voice).await;
        let terminal = Caller::human(OWNER, SurfaceKind::Terminal, Origin::Keyboard)
            .with_groups(["owner", "operator"]);
        let mut link = voice.hub.open_in_thread(&terminal, "scripted").await.unwrap();
        speak(&link).await;
        let (id, _) = prompted(&mut link).await;
        assert_ne!(id, finance.id, "another session's decision is not prompted here");
        assert_eq!(voice.engine.decision(&web(), &id).await.unwrap().session_id, link.session_id);
    }

    /// Only the caller's own voice conversation continues; anyone else's is refused (S-I8).
    #[tokio::test]
    async fn only_your_own_voice_session_continues() {
        let voice = voice(Vec::new()).await;
        let mine = voice.hub.session_for(&web(), None).await.unwrap();
        assert_eq!(voice.hub.session_for(&web(), Some(&mine)).await.unwrap(), mine);
        let bo = Caller::human("bo", SurfaceKind::Web, Origin::Pointer).with_groups(["owner"]);
        let error = voice.hub.session_for(&bo, Some(&mine)).await.unwrap_err();
        assert_eq!(error.code(), "surface.voice.not_your_session");
        // A text session of the caller's is not a voice conversation.
        voice
            .sessions
            .create(CreateRequest {
                app_name: APP.into(),
                user_id: OWNER.into(),
                session_id: Some("typed".into()),
                state: Default::default(),
            })
            .await
            .unwrap();
        let error = voice.hub.session_for(&web(), Some("typed")).await.unwrap_err();
        assert_eq!(error.code(), "surface.voice.not_your_session");
    }
}
