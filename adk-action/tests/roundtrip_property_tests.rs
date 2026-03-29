//! Round-trip property tests for `adk-action` types.
//!
//! These tests verify that serializing to JSON and deserializing back
//! produces values equal to the original for all action node types.

use adk_action::*;
use proptest::prelude::*;
use std::collections::HashMap;

// ── Helper: build a StandardProperties instance ───────────────────────

fn make_standard(id: &str, name: &str, output_key: &str) -> StandardProperties {
    StandardProperties {
        id: id.to_string(),
        name: name.to_string(),
        description: Some("test description".to_string()),
        position: Some((100.0, 200.0)),
        error_handling: ErrorHandling {
            mode: ErrorMode::Retry,
            retry_count: Some(3),
            retry_delay: Some(1000),
            fallback_value: None,
        },
        tracing: Tracing { enabled: true, log_level: LogLevel::Debug },
        callbacks: Callbacks {
            on_start: Some("on_start_hook".to_string()),
            on_complete: None,
            on_error: Some("on_error_hook".to_string()),
        },
        execution: ExecutionControl { timeout: 5000, condition: Some("{{enabled}}".to_string()) },
        mapping: InputOutputMapping {
            input_mapping: Some(HashMap::from([("key1".to_string(), "val1".to_string())])),
            output_key: output_key.to_string(),
        },
    }
}

fn make_minimal_standard(id: &str) -> StandardProperties {
    StandardProperties {
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        position: None,
        error_handling: ErrorHandling {
            mode: ErrorMode::Stop,
            retry_count: None,
            retry_delay: None,
            fallback_value: None,
        },
        tracing: Tracing { enabled: false, log_level: LogLevel::None },
        callbacks: Callbacks { on_start: None, on_complete: None, on_error: None },
        execution: ExecutionControl { timeout: 30000, condition: None },
        mapping: InputOutputMapping { input_mapping: None, output_key: "result".to_string() },
    }
}

// ── Generators for StandardProperties fields ──────────────────────────

fn arb_error_mode() -> impl Strategy<Value = ErrorMode> {
    prop_oneof![
        Just(ErrorMode::Stop),
        Just(ErrorMode::Continue),
        Just(ErrorMode::Retry),
        Just(ErrorMode::Fallback),
    ]
}

fn arb_log_level() -> impl Strategy<Value = LogLevel> {
    prop_oneof![
        Just(LogLevel::None),
        Just(LogLevel::Error),
        Just(LogLevel::Info),
        Just(LogLevel::Debug),
    ]
}

fn arb_error_handling() -> impl Strategy<Value = ErrorHandling> {
    (arb_error_mode(), any::<bool>(), 0u32..10, 0u64..5000).prop_map(
        |(mode, has_retry, count, delay)| ErrorHandling {
            mode,
            retry_count: if has_retry { Some(count) } else { None },
            retry_delay: if has_retry { Some(delay) } else { None },
            fallback_value: None,
        },
    )
}

fn arb_standard_properties() -> impl Strategy<Value = StandardProperties> {
    (
        "[a-z][a-z0-9_]{2,10}",
        "[A-Za-z ]{3,20}",
        any::<bool>(),
        arb_error_handling(),
        arb_log_level(),
        any::<bool>(),
    )
        .prop_map(|(id, name, has_desc, error_handling, log_level, tracing_enabled)| {
            StandardProperties {
                id: id.clone(),
                name,
                description: if has_desc { Some("a description".to_string()) } else { None },
                position: Some((50.0, 75.0)),
                error_handling,
                tracing: Tracing { enabled: tracing_enabled, log_level },
                callbacks: Callbacks { on_start: None, on_complete: None, on_error: None },
                execution: ExecutionControl { timeout: 30000, condition: None },
                mapping: InputOutputMapping {
                    input_mapping: None,
                    output_key: format!("{id}_result"),
                },
            }
        })
}

// ── Property 2: StandardProperties Round-Trip ─────────────────────────

// **Feature: action-node-graph-standardization, Property 2: StandardProperties Round-Trip**
// *For any* valid StandardProperties value, serializing to JSON and deserializing back
// SHALL produce a value equal to the original.
// **Validates: Requirements 2.1-2.5, 11.1**
proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn prop_standard_properties_roundtrip(sp in arb_standard_properties()) {
        let json = serde_json::to_string(&sp).expect("serialize");
        let deserialized: StandardProperties = serde_json::from_str(&json).expect("deserialize");
        prop_assert_eq!(deserialized, sp);
    }
}

// ── Concrete instances for all 14 ActionNodeConfig variants ───────────

fn make_trigger_manual() -> ActionNodeConfig {
    ActionNodeConfig::Trigger(TriggerNodeConfig {
        standard: make_standard("trigger_1", "Manual Trigger", "trigger_out"),
        trigger_type: TriggerType::Manual,
        manual: Some(ManualTriggerConfig {
            input_label: Some("Enter prompt".to_string()),
            default_prompt: Some("Hello".to_string()),
        }),
        webhook: None,
        schedule: None,
        event: None,
    })
}

fn make_trigger_webhook() -> ActionNodeConfig {
    ActionNodeConfig::Trigger(TriggerNodeConfig {
        standard: make_minimal_standard("trigger_wh"),
        trigger_type: TriggerType::Webhook,
        manual: None,
        webhook: Some(WebhookConfig {
            path: "/api/hook".to_string(),
            method: Some(HttpMethod::Post),
            auth: Some(WebhookAuthConfig {
                auth_type: "bearer".to_string(),
                token: Some("secret123".to_string()),
                header_name: None,
                api_key: None,
            }),
        }),
        schedule: None,
        event: None,
    })
}

fn make_trigger_schedule() -> ActionNodeConfig {
    ActionNodeConfig::Trigger(TriggerNodeConfig {
        standard: make_minimal_standard("trigger_sched"),
        trigger_type: TriggerType::Schedule,
        manual: None,
        webhook: None,
        schedule: Some(ScheduleConfig {
            cron: "0 */5 * * *".to_string(),
            timezone: Some("UTC".to_string()),
            default_prompt: Some("run scheduled".to_string()),
        }),
        event: None,
    })
}

fn make_trigger_event() -> ActionNodeConfig {
    ActionNodeConfig::Trigger(TriggerNodeConfig {
        standard: make_minimal_standard("trigger_evt"),
        trigger_type: TriggerType::Event,
        manual: None,
        webhook: None,
        schedule: None,
        event: Some(EventConfig {
            source: "github".to_string(),
            event_type: "push".to_string(),
            filter: Some("$.ref == 'refs/heads/main'".to_string()),
        }),
    })
}

fn make_http_node() -> ActionNodeConfig {
    ActionNodeConfig::Http(HttpNodeConfig {
        standard: make_standard("http_1", "Fetch API", "api_response"),
        method: HttpMethod::Post,
        url: "https://api.example.com/data".to_string(),
        auth: HttpAuth::Bearer(BearerAuth { token: "tok_abc".to_string() }),
        headers: HashMap::from([("Content-Type".to_string(), "application/json".to_string())]),
        body: HttpBody::Json { data: serde_json::json!({"key": "value"}) },
        response: HttpResponse {
            response_type: "json".to_string(),
            status_validation: Some("200-299".to_string()),
        },
        rate_limit: Some(RateLimit { max_requests: 100, window_ms: 60000 }),
    })
}

fn make_set_node() -> ActionNodeConfig {
    ActionNodeConfig::Set(SetNodeConfig {
        standard: make_standard("set_1", "Set Variables", "set_result"),
        mode: SetMode::Set,
        variables: vec![
            Variable {
                key: "api_key".to_string(),
                value: serde_json::json!("secret"),
                value_type: "string".to_string(),
                is_secret: true,
            },
            Variable {
                key: "count".to_string(),
                value: serde_json::json!(42),
                value_type: "number".to_string(),
                is_secret: false,
            },
        ],
        env_vars: Some(EnvVarsConfig {
            load_from_env: true,
            prefix: Some("APP_".to_string()),
            keys: vec!["DATABASE_URL".to_string()],
        }),
    })
}

fn make_transform_node() -> ActionNodeConfig {
    ActionNodeConfig::Transform(TransformNodeConfig {
        standard: make_standard("transform_1", "Transform Data", "transformed"),
        transform_type: TransformType::Template,
        expression: None,
        template: Some("Hello {{user.name}}!".to_string()),
        builtin: None,
        coercion: Some(TypeCoercion {
            from_type: "string".to_string(),
            to_type: "number".to_string(),
        }),
    })
}

fn make_switch_node() -> ActionNodeConfig {
    ActionNodeConfig::Switch(SwitchNodeConfig {
        standard: make_standard("switch_1", "Route Decision", "switch_result"),
        evaluation_mode: EvaluationMode::FirstMatch,
        conditions: vec![
            SwitchCondition {
                id: "cond_1".to_string(),
                name: "Is Admin".to_string(),
                expression: ExpressionMode {
                    field: "user.role".to_string(),
                    operator: "eq".to_string(),
                    value: "admin".to_string(),
                },
                output_port: "admin_branch".to_string(),
            },
            SwitchCondition {
                id: "cond_2".to_string(),
                name: "Is User".to_string(),
                expression: ExpressionMode {
                    field: "user.role".to_string(),
                    operator: "eq".to_string(),
                    value: "user".to_string(),
                },
                output_port: "user_branch".to_string(),
            },
        ],
        default_branch: Some("fallback".to_string()),
    })
}

fn make_loop_node() -> ActionNodeConfig {
    ActionNodeConfig::Loop(LoopNodeConfig {
        standard: make_standard("loop_1", "Process Items", "loop_result"),
        loop_type: LoopType::ForEach,
        for_each: Some(ForEachConfig {
            source: "items".to_string(),
            item_var: "item".to_string(),
            index_var: "index".to_string(),
        }),
        while_config: None,
        times: None,
        parallel: Some(ParallelConfig {
            enabled: true,
            batch_size: Some(5),
            delay_between: Some(100),
        }),
        results: Some(ResultsConfig {
            collect: true,
            aggregation_key: Some("processed_items".to_string()),
        }),
    })
}

fn make_merge_node() -> ActionNodeConfig {
    ActionNodeConfig::Merge(MergeNodeConfig {
        standard: make_standard("merge_1", "Merge Results", "merged"),
        mode: MergeMode::WaitAll,
        required_count: None,
        combine_strategy: CombineStrategy::Array,
        timeout: Some(MergeTimeout { timeout_ms: 30000, on_timeout: "continue".to_string() }),
    })
}

fn make_wait_node() -> ActionNodeConfig {
    ActionNodeConfig::Wait(WaitNodeConfig {
        standard: make_standard("wait_1", "Wait", "wait_result"),
        wait_type: WaitType::Fixed,
        fixed: Some(FixedDuration { duration: 5000, unit: "ms".to_string() }),
        until: None,
        webhook: None,
        condition: None,
    })
}

fn make_code_node() -> ActionNodeConfig {
    ActionNodeConfig::Code(CodeNodeConfig {
        standard: make_standard("code_1", "Run Code", "code_result"),
        language: CodeLanguage::Rust,
        code: "fn run(input: Value) -> Value { input }".to_string(),
        sandbox: Some(SandboxConfig {
            memory_limit_mb: Some(256),
            time_limit_ms: Some(5000),
            allow_network: false,
            allow_fs: false,
        }),
    })
}

fn make_database_node() -> ActionNodeConfig {
    ActionNodeConfig::Database(DatabaseNodeConfig {
        standard: make_standard("db_1", "Query DB", "db_result"),
        connection: DatabaseConnection {
            database_type: DatabaseType::Postgresql,
            connection_string: Some("postgres://localhost/mydb".to_string()),
            credential_ref: None,
        },
        sql: Some(SqlConfig {
            query: "SELECT * FROM users WHERE id = $1".to_string(),
            params: vec![serde_json::json!(1)],
            operation: "query".to_string(),
        }),
        mongo: None,
        redis: None,
    })
}

fn make_email_node() -> ActionNodeConfig {
    ActionNodeConfig::Email(EmailNodeConfig {
        standard: make_standard("email_1", "Send Email", "email_result"),
        mode: EmailMode::Send,
        imap: None,
        filter: None,
        smtp: Some(SmtpConfig {
            host: "smtp.example.com".to_string(),
            port: 587,
            username: "user@example.com".to_string(),
            credential_ref: Some("smtp_cred".to_string()),
            tls: true,
        }),
        recipients: Some(EmailRecipients {
            to: vec!["recipient@example.com".to_string()],
            cc: vec![],
            bcc: vec![],
        }),
        content: Some(EmailContent {
            subject: "Test".to_string(),
            body: "Hello!".to_string(),
            body_type: EmailBodyType::Text,
        }),
        attachments: vec![EmailAttachment {
            filename: "report.pdf".to_string(),
            state_key: "report_data".to_string(),
        }],
    })
}

fn make_notification_node() -> ActionNodeConfig {
    ActionNodeConfig::Notification(NotificationNodeConfig {
        standard: make_standard("notif_1", "Notify Slack", "notif_result"),
        notification_channel: NotificationChannel::Slack,
        webhook_url: "https://hooks.slack.com/services/xxx".to_string(),
        message: NotificationMessage {
            text: "Build {{status}}: {{project}}".to_string(),
            format: Some(MessageFormat::Markdown),
            username: Some("CI Bot".to_string()),
            icon_url: Some("https://example.com/icon.png".to_string()),
            channel: Some("#builds".to_string()),
        },
    })
}

fn make_rss_node() -> ActionNodeConfig {
    ActionNodeConfig::Rss(RssNodeConfig {
        standard: make_standard("rss_1", "Monitor Feed", "rss_result"),
        feed_url: "https://blog.example.com/feed.xml".to_string(),
        filter: Some(FeedFilter {
            keywords: vec!["rust".to_string(), "ai".to_string()],
            author: Some("Alice".to_string()),
            since: Some("2024-01-01".to_string()),
            categories: vec!["tech".to_string()],
        }),
        seen_tracking: Some(SeenItemTracking {
            enabled: true,
            state_key: Some("seen_rss_ids".to_string()),
            max_items: Some(500),
        }),
    })
}

fn make_file_node() -> ActionNodeConfig {
    ActionNodeConfig::File(FileNodeConfig {
        standard: make_standard("file_1", "Read File", "file_result"),
        operation: FileOperation::Read,
        local: Some(LocalFileConfig { path: "/tmp/data.json".to_string() }),
        cloud: None,
        parse: Some(FileParseConfig { format: FileFormat::Json, csv_options: None }),
        write: None,
        list: None,
    })
}

// ── Property 3: ActionNodeConfig Tagged Union Round-Trip ──────────────

// **Feature: action-node-graph-standardization, Property 3: ActionNodeConfig Tagged Union Round-Trip**
// *For any* valid ActionNodeConfig value, serializing to JSON with the type tag and
// deserializing back SHALL produce a value equal to the original, preserving the type
// tag and all fields for all 14 node types.
// **Validates: Requirements 1.4, 11.1**

fn all_action_node_configs() -> Vec<ActionNodeConfig> {
    vec![
        make_trigger_manual(),
        make_trigger_webhook(),
        make_trigger_schedule(),
        make_trigger_event(),
        make_http_node(),
        make_set_node(),
        make_transform_node(),
        make_switch_node(),
        make_loop_node(),
        make_merge_node(),
        make_wait_node(),
        make_code_node(),
        make_database_node(),
        make_email_node(),
        make_notification_node(),
        make_rss_node(),
        make_file_node(),
    ]
}

/// Generator that picks one of the 14+ concrete ActionNodeConfig variants.
fn arb_action_node_config() -> impl Strategy<Value = ActionNodeConfig> {
    let configs = all_action_node_configs();
    (0..configs.len()).prop_map(move |idx| configs[idx].clone())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn prop_action_node_config_roundtrip(config in arb_action_node_config()) {
        let json = serde_json::to_string(&config).expect("serialize");
        let deserialized: ActionNodeConfig = serde_json::from_str(&json).expect("deserialize");
        prop_assert_eq!(&deserialized, &config);

        // Verify the type tag is preserved
        let json_value: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        let type_tag = json_value.get("type").expect("type tag present");
        prop_assert_eq!(type_tag.as_str().unwrap(), config.node_type());
    }
}

// ── Property 3 (exhaustive): every variant round-trips ────────────────

#[test]
fn test_all_14_node_types_roundtrip() {
    let configs = all_action_node_configs();
    // We have 17 configs covering all 14 types (4 trigger variants + 13 other types)
    assert!(configs.len() >= 14, "must cover all node types");

    let mut seen_types = std::collections::HashSet::new();
    for config in &configs {
        seen_types.insert(config.node_type());

        let json = serde_json::to_string(config).expect("serialize");
        let deserialized: ActionNodeConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&deserialized, config, "round-trip failed for {}", config.node_type());

        // Verify type tag
        let json_value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let type_tag = json_value["type"].as_str().expect("type tag");
        assert_eq!(type_tag, config.node_type());
    }

    // Verify all 14 types are covered
    let expected_types = [
        "trigger",
        "http",
        "set",
        "transform",
        "switch",
        "loop",
        "merge",
        "wait",
        "code",
        "database",
        "email",
        "notification",
        "rss",
        "file",
    ];
    for t in &expected_types {
        assert!(seen_types.contains(t), "missing type: {t}");
    }
}

// ── Property 3: verify pretty-printed JSON also round-trips ───────────

#[test]
fn test_pretty_json_roundtrip() {
    for config in all_action_node_configs() {
        let pretty = serde_json::to_string_pretty(&config).expect("pretty serialize");
        let deserialized: ActionNodeConfig =
            serde_json::from_str(&pretty).expect("deserialize pretty");
        assert_eq!(&deserialized, &config, "pretty round-trip failed for {}", config.node_type());
    }
}

// ── Property 9: TriggerConfig Round-Trip ──────────────────────────────

// **Feature: action-node-graph-standardization, Property 9: TriggerConfig Round-Trip**
// *For any* valid TriggerNodeConfig (manual, webhook, schedule, event), serializing to
// JSON and deserializing back SHALL produce a value equal to the original, preserving
// the trigger type and all sub-configuration.
// **Validates: Requirements 15.1, 16.1, 17.1, 18.1**

fn all_trigger_configs() -> Vec<TriggerNodeConfig> {
    vec![
        // Manual trigger
        TriggerNodeConfig {
            standard: make_standard("t_manual", "Manual", "trigger_out"),
            trigger_type: TriggerType::Manual,
            manual: Some(ManualTriggerConfig {
                input_label: Some("Prompt".to_string()),
                default_prompt: Some("default".to_string()),
            }),
            webhook: None,
            schedule: None,
            event: None,
        },
        // Manual trigger with no sub-config
        TriggerNodeConfig {
            standard: make_minimal_standard("t_manual_bare"),
            trigger_type: TriggerType::Manual,
            manual: None,
            webhook: None,
            schedule: None,
            event: None,
        },
        // Webhook trigger
        TriggerNodeConfig {
            standard: make_standard("t_webhook", "Webhook", "trigger_out"),
            trigger_type: TriggerType::Webhook,
            manual: None,
            webhook: Some(WebhookConfig {
                path: "/hooks/deploy".to_string(),
                method: Some(HttpMethod::Post),
                auth: Some(WebhookAuthConfig {
                    auth_type: "api_key".to_string(),
                    token: None,
                    header_name: Some("X-API-Key".to_string()),
                    api_key: Some("key123".to_string()),
                }),
            }),
            schedule: None,
            event: None,
        },
        // Schedule trigger
        TriggerNodeConfig {
            standard: make_standard("t_schedule", "Cron Job", "trigger_out"),
            trigger_type: TriggerType::Schedule,
            manual: None,
            webhook: None,
            schedule: Some(ScheduleConfig {
                cron: "0 0 * * MON-FRI".to_string(),
                timezone: Some("America/New_York".to_string()),
                default_prompt: None,
            }),
            event: None,
        },
        // Event trigger
        TriggerNodeConfig {
            standard: make_standard("t_event", "GitHub Push", "trigger_out"),
            trigger_type: TriggerType::Event,
            manual: None,
            webhook: None,
            schedule: None,
            event: Some(EventConfig {
                source: "github".to_string(),
                event_type: "push".to_string(),
                filter: Some("$.ref == 'refs/heads/main'".to_string()),
            }),
        },
        // Event trigger with no filter
        TriggerNodeConfig {
            standard: make_minimal_standard("t_event_nofilter"),
            trigger_type: TriggerType::Event,
            manual: None,
            webhook: None,
            schedule: None,
            event: Some(EventConfig {
                source: "internal".to_string(),
                event_type: "user.created".to_string(),
                filter: None,
            }),
        },
    ]
}

fn arb_trigger_config() -> impl Strategy<Value = TriggerNodeConfig> {
    let configs = all_trigger_configs();
    (0..configs.len()).prop_map(move |idx| configs[idx].clone())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn prop_trigger_config_roundtrip(trigger in arb_trigger_config()) {
        let json = serde_json::to_string(&trigger).expect("serialize");
        let deserialized: TriggerNodeConfig = serde_json::from_str(&json).expect("deserialize");
        prop_assert_eq!(&deserialized.trigger_type, &trigger.trigger_type);
        prop_assert_eq!(&deserialized.manual, &trigger.manual);
        prop_assert_eq!(&deserialized.webhook, &trigger.webhook);
        prop_assert_eq!(&deserialized.schedule, &trigger.schedule);
        prop_assert_eq!(&deserialized.event, &trigger.event);
        prop_assert_eq!(&deserialized, &trigger);
    }
}

/// Exhaustive test: every trigger type round-trips with sub-config preserved.
#[test]
fn test_all_trigger_types_roundtrip() {
    let triggers = all_trigger_configs();
    let mut seen_types = std::collections::HashSet::new();

    for trigger in &triggers {
        let type_str = serde_json::to_value(&trigger.trigger_type)
            .expect("serialize trigger type")
            .as_str()
            .expect("trigger type is string")
            .to_string();
        seen_types.insert(type_str);

        let json = serde_json::to_string(trigger).expect("serialize");
        let deserialized: TriggerNodeConfig = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(&deserialized.trigger_type, &trigger.trigger_type);
        assert_eq!(
            &deserialized, trigger,
            "trigger round-trip failed for {:?}",
            trigger.trigger_type
        );
    }

    // All 4 trigger types covered
    assert!(seen_types.contains("manual"));
    assert!(seen_types.contains("webhook"));
    assert!(seen_types.contains("schedule"));
    assert!(seen_types.contains("event"));
}

// ── Additional round-trip edge cases ──────────────────────────────────

#[test]
fn test_http_auth_variants_roundtrip() {
    let auths = vec![
        HttpAuth::None,
        HttpAuth::Bearer(BearerAuth { token: "tok".to_string() }),
        HttpAuth::Basic(BasicAuth { username: "user".to_string(), password: "pass".to_string() }),
        HttpAuth::ApiKey(ApiKeyAuth { header: "X-Key".to_string(), value: "val".to_string() }),
    ];

    for auth in &auths {
        let json = serde_json::to_string(auth).expect("serialize");
        let deserialized: HttpAuth = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&deserialized, auth);
    }
}

#[test]
fn test_http_body_variants_roundtrip() {
    let bodies = vec![
        HttpBody::None,
        HttpBody::Json { data: serde_json::json!({"a": 1}) },
        HttpBody::Form { fields: HashMap::from([("field".to_string(), "value".to_string())]) },
        HttpBody::Raw { content: "raw body".to_string(), content_type: "text/plain".to_string() },
    ];

    for body in &bodies {
        let json = serde_json::to_string(body).expect("serialize");
        let deserialized: HttpBody = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&deserialized, body);
    }
}
