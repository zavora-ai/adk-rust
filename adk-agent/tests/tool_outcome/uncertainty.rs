use super::*;

fn cancelled_error() -> AdkError {
    AdkError::new(ErrorComponent::Tool, ErrorCategory::Cancelled, "tool.cancelled", "stopped")
}

fn answer(value: Value) -> Part {
    Part::FunctionResponse {
        function_response: adk_core::FunctionResponseData::new("charge_card", value),
        id: Some("call-1".to_string()),
        annotations: Some(
            json!({TOOL_RESULT_STATUS_KEY: ToolResultStatus::OutcomeUnknown, TOOL_RESULT_ERROR_CODE_KEY: "tool.timeout", TOOL_RESULT_ERROR_CATEGORY_KEY: "timeout"}),
        ),
    }
}

/// The ledger's answer to a replay, which names no error.
fn replayed(mut part: Part) -> Part {
    part.set_tool_result_status(ToolResultStatus::OutcomeUnknown, None);
    part
}

fn replay_answer() -> Part {
    replayed(answer(adk_core::outcome_unknown_response(
        "an earlier attempt of this call to 'charge_card' began but recorded no outcome, so it may have taken effect; it was not executed again",
    )))
}

async fn check_replay(
    outcome: Run,
    model: &ScriptedModel,
    runs: &AtomicUsize,
    ledger: &InMemoryActionLedger,
    expected: [Part; 2],
) {
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let stored: Vec<_> = outcome
        .session_events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| &content.parts)
        .filter(|part| matches!(part, Part::FunctionResponse { .. }))
        .collect();
    assert_eq!(stored, expected.iter().collect::<Vec<_>>());
    assert!(
        stored
            .iter()
            .all(|part| { part.tool_result_status() == Some(ToolResultStatus::OutcomeUnknown) })
    );
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    let sent: Vec<_> = requests[2]
        .contents
        .iter()
        .flat_map(|content| &content.parts)
        .filter(|part| matches!(part, Part::FunctionResponse { .. }))
        .collect();
    assert_eq!(sent, stored);
    let key = format!("outcomes/user/session/{}/call-1", outcome.invocation_id());
    let record = ledger.get(&key).await.unwrap().expect("ledger record");
    assert_eq!(record.outcome, None);
}

#[tokio::test]
async fn after_errors_keep_unknown() {
    for error in [audit_error as fn() -> AdkError, cancelled_error] {
        for stage in stages() {
            let model = ScriptedModel::new(vec![
                call("charge_card", json!({})),
                call("charge_card", json!({})),
            ]);
            let (mut tool, runs) = CountingTool::ok("charge_card", json!({"charged": true}));
            tool.delay = Some(Duration::from_secs(5));
            let mut builder = LlmAgentBuilder::new("payments")
                .model(model.clone())
                .tool(Arc::new(tool))
                .tool_timeout(Duration::from_millis(50));
            let mut plugin = None;
            match stage {
                AfterStage::Callback => {
                    builder = builder.after_tool_callback(Box::new(move |_| {
                        Box::pin(async move { Err(error()) })
                    }));
                }
                AfterStage::CallbackFull => {
                    builder = builder.after_tool_callback_full(Box::new(move |_, _, _, _| {
                        Box::pin(async move { Err(error()) })
                    }));
                }
                AfterStage::RunnerHook => {
                    plugin = Some(Plugin::new(PluginConfig {
                        name: "audit".into(),
                        after_tool: Some(Box::new(move |_| Box::pin(async move { Err(error()) }))),
                        ..Default::default()
                    }));
                }
                #[cfg(feature = "enhanced-plugins")]
                AfterStage::EnhancedPlugin => {
                    builder = builder.enhanced_plugin(Arc::new(FailingAfterPlugin(error)));
                }
            }
            let ledger = Arc::new(InMemoryActionLedger::new());
            let config = RunConfig::builder().action_ledger(ledger.clone()).build();
            let outcome = run(builder.build().unwrap(), plugin, config).await;
            check_replay(
                outcome,
                &model,
                &runs,
                &ledger,
                [
                    answer(adk_core::result_withheld_response("charge_card", &error(), false)),
                    replay_answer(),
                ],
            )
            .await;
        }
    }
}

#[tokio::test]
async fn error_fallback_keeps_unknown() {
    let value = json!({"isError": true, "error": "domain row"});
    for runner_hook in [false, true] {
        let model = ScriptedModel::new(vec![
            call("charge_card", json!({})),
            call("charge_card", json!({})),
        ]);
        let (mut tool, runs) = CountingTool::ok("charge_card", json!({"charged": true}));
        tool.delay = Some(Duration::from_secs(5));
        let mut builder = LlmAgentBuilder::new("payments")
            .model(model.clone())
            .tool(Arc::new(tool))
            .tool_timeout(Duration::from_millis(50));
        let callbacks = Arc::new(AtomicUsize::new(0));
        let seen = callbacks.clone();
        let replacement = value.clone();
        let callback: adk_core::OnToolErrorCallback = Box::new(move |ctx, _, _, _| {
            assert!(!ctx.tool_outcome().expect("timed-out call").success);
            seen.fetch_add(1, Ordering::SeqCst);
            let response = replacement.clone();
            Box::pin(async move { Ok(Some(response)) })
        });
        let mut plugin = None;
        if runner_hook {
            plugin = Some(Plugin::new(PluginConfig {
                name: "fallback".into(),
                on_tool_error: Some(callback),
                ..Default::default()
            }));
        } else {
            builder = builder.on_tool_error(callback);
        }
        let ledger = Arc::new(InMemoryActionLedger::new());
        let config = RunConfig::builder().action_ledger(ledger.clone()).build();
        let outcome = run(builder.build().unwrap(), plugin, config).await;
        assert_eq!(callbacks.load(Ordering::SeqCst), 1);
        check_replay(outcome, &model, &runs, &ledger, [answer(value.clone()), replay_answer()])
            .await;
    }
}

#[tokio::test]
async fn callback_replacements_keep_metadata() {
    let value = json!({"isError": true, "error": "domain row"});
    let model =
        ScriptedModel::new(vec![call("charge_card", json!({})), call("charge_card", json!({}))]);
    let (mut tool, runs) = CountingTool::ok("charge_card", json!({"charged": true}));
    tool.delay = Some(Duration::from_secs(5));
    let callbacks = Arc::new(AtomicUsize::new(0));
    let seen = callbacks.clone();
    let replacement = value.clone();
    let full_replacement = value.clone();
    let builder = LlmAgentBuilder::new("payments")
        .model(model.clone())
        .tool(Arc::new(tool))
        .tool_timeout(Duration::from_millis(50))
        .after_tool_callback(Box::new(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
            let response = Content {
                role: "function".to_string(),
                parts: vec![Part::FunctionResponse {
                    function_response: adk_core::FunctionResponseData::new(
                        "replacement",
                        replacement.clone(),
                    ),
                    id: Some("other-call".to_string()),
                    annotations: Some(json!({"trace": {"source": "callback"}})),
                }],
            };
            Box::pin(async move { Ok(Some(response)) })
        }))
        .after_tool_callback_full(Box::new(move |_, _, _, _| {
            let response = full_replacement.clone();
            Box::pin(async move { Ok(Some(response)) })
        }));
    #[cfg(feature = "enhanced-plugins")]
    let builder = builder.enhanced_plugin(Arc::new(OutcomeRecordingPlugin(Arc::default())));
    let ledger = Arc::new(InMemoryActionLedger::new());
    let config = RunConfig::builder().action_ledger(ledger.clone()).build();
    let outcome = run(builder.build().unwrap(), None, config).await;
    assert_eq!(callbacks.load(Ordering::SeqCst), 1);
    let mut expected = answer(value);
    if let Part::FunctionResponse { annotations, .. } = &mut expected {
        *annotations = Some(json!({
            TOOL_RESULT_STATUS_KEY: ToolResultStatus::OutcomeUnknown,
            TOOL_RESULT_ERROR_CODE_KEY: "tool.timeout",
            TOOL_RESULT_ERROR_CATEGORY_KEY: "timeout",
            "trace": {"source": "callback"}
        }));
    }
    check_replay(outcome, &model, &runs, &ledger, [expected, replay_answer()]).await;
}

#[cfg(feature = "guardrails")]
struct WithholdResults(Arc<AtomicUsize>);

#[cfg(feature = "guardrails")]
#[async_trait]
impl adk_agent::guardrails::ToolGuardrail for WithholdResults {
    fn name(&self) -> &str {
        "withhold_results"
    }

    async fn validate_result(
        &self,
        _tool: &str,
        _args: &Value,
        _result: &Value,
    ) -> adk_agent::guardrails::ToolResultGuardrailResult {
        self.0.fetch_add(1, Ordering::SeqCst);
        adk_agent::guardrails::ToolResultGuardrailResult::withhold(
            "results are under review",
            adk_agent::guardrails::Severity::High,
        )
    }
}

#[cfg(feature = "guardrails")]
#[tokio::test]
async fn withheld_results_keep_unknown() {
    let model =
        ScriptedModel::new(vec![call("charge_card", json!({})), call("charge_card", json!({}))]);
    let (mut tool, runs) = CountingTool::ok("charge_card", json!({"charged": true}));
    tool.delay = Some(Duration::from_secs(5));
    let screened = Arc::new(AtomicUsize::new(0));
    let agent = LlmAgentBuilder::new("payments")
        .model(model.clone())
        .tool(Arc::new(tool))
        .tool_timeout(Duration::from_millis(50))
        .tool_guardrails(
            adk_agent::guardrails::ToolGuardrailSet::new().with(WithholdResults(screened.clone())),
        )
        .build()
        .unwrap();
    let ledger = Arc::new(InMemoryActionLedger::new());
    let config = RunConfig::builder().action_ledger(ledger.clone()).build();
    let outcome = run(agent, None, config).await;
    assert_eq!(screened.load(Ordering::SeqCst), 2);
    let expected = answer(adk_core::result_blocked_response(
        "charge_card",
        "tool guardrail 'withhold_results' (High): results are under review",
    ));
    check_replay(outcome, &model, &runs, &ledger, [expected.clone(), replayed(expected)]).await;
}
