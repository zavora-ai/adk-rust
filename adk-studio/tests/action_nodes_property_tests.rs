//! Property-based tests for Action Node Code Generation
//!
//! **Property 10: Code Generation Validity**
//! **Validates: Requirements 13.1**
//!
//! For any valid action node configuration, the generated code SHALL:
//! - Have balanced braces and parentheses
//! - Not contain syntax errors
//! - Pass basic validation checks

use adk_studio::codegen::action_nodes::*;
use proptest::prelude::*;
use std::collections::HashMap;

// ============================================
// Arbitrary Generators for Action Node Types
// ============================================

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

fn arb_identifier() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{2,15}".prop_map(|s| s)
}

fn arb_safe_string() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 _-]{0,50}".prop_map(|s| s)
}

fn arb_url() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("https://api.example.com/data".to_string()),
        Just("https://httpbin.org/get".to_string()),
        Just("https://jsonplaceholder.typicode.com/posts".to_string()),
        "[a-z]{3,10}\\.[a-z]{2,5}".prop_map(|s| format!("https://{}/api", s)),
    ]
}

fn arb_standard_properties() -> impl Strategy<Value = StandardProperties> {
    (
        arb_identifier(),
        arb_safe_string(),
        arb_error_mode(),
        arb_log_level(),
        0u64..60000u64,
        arb_identifier(),
    )
        .prop_map(|(id, name, error_mode, log_level, timeout, output_key)| {
            StandardProperties {
                id: id.clone(),
                name,
                description: None,
                error_handling: ErrorHandling {
                    mode: error_mode,
                    retry_count: Some(3),
                    retry_delay: Some(1000),
                    fallback_value: Some(serde_json::json!(null)),
                },
                tracing: Tracing { enabled: true, log_level },
                callbacks: Callbacks::default(),
                execution: ExecutionControl { timeout, condition: None },
                mapping: InputOutputMapping { input_mapping: None, output_key },
            }
        })
}

// ============================================
// Trigger Node Generators
// ============================================

fn arb_trigger_type() -> impl Strategy<Value = TriggerType> {
    prop_oneof![
        Just(TriggerType::Manual),
        Just(TriggerType::Webhook),
        Just(TriggerType::Schedule),
        Just(TriggerType::Event),
    ]
}

fn arb_trigger_node_config() -> impl Strategy<Value = TriggerNodeConfig> {
    (arb_standard_properties(), arb_trigger_type()).prop_map(|(standard, trigger_type)| {
        let webhook = if trigger_type == TriggerType::Webhook {
            Some(WebhookConfig {
                path: "/webhook".to_string(),
                method: "POST".to_string(),
                auth: "none".to_string(),
                auth_config: None,
            })
        } else {
            None
        };
        let schedule = if trigger_type == TriggerType::Schedule {
            Some(ScheduleConfig { cron: "0 * * * *".to_string(), timezone: "UTC".to_string(), default_prompt: None })
        } else {
            None
        };
        let event = if trigger_type == TriggerType::Event {
            Some(EventConfig {
                source: "system".to_string(),
                event_type: "notification".to_string(),
                filter: None,
            })
        } else {
            None
        };
        TriggerNodeConfig { standard, trigger_type, webhook, schedule, event }
    })
}

// ============================================
// HTTP Node Generators
// ============================================

fn arb_http_method() -> impl Strategy<Value = HttpMethod> {
    prop_oneof![
        Just(HttpMethod::Get),
        Just(HttpMethod::Post),
        Just(HttpMethod::Put),
        Just(HttpMethod::Patch),
        Just(HttpMethod::Delete),
    ]
}

fn arb_http_node_config() -> impl Strategy<Value = HttpNodeConfig> {
    (arb_standard_properties(), arb_http_method(), arb_url()).prop_map(|(standard, method, url)| {
        HttpNodeConfig {
            standard,
            method,
            url,
            auth: HttpAuth {
                auth_type: "none".to_string(),
                bearer: None,
                basic: None,
                api_key: None,
            },
            headers: HashMap::new(),
            body: HttpBody { body_type: "none".to_string(), content: None },
            response: HttpResponse {
                response_type: "json".to_string(),
                status_validation: Some("200-299".to_string()),
                json_path: None,
            },
            rate_limit: None,
        }
    })
}

// ============================================
// Set Node Generators
// ============================================

fn arb_set_mode() -> impl Strategy<Value = SetMode> {
    prop_oneof![Just(SetMode::Set), Just(SetMode::Merge), Just(SetMode::Delete),]
}

fn arb_set_node_config() -> impl Strategy<Value = SetNodeConfig> {
    (arb_standard_properties(), arb_set_mode(), arb_identifier()).prop_map(
        |(standard, mode, var_key)| SetNodeConfig {
            standard,
            mode,
            variables: vec![Variable {
                key: var_key,
                value: serde_json::json!("test_value"),
                value_type: "string".to_string(),
                is_secret: false,
            }],
            env_vars: None,
        },
    )
}

// ============================================
// Transform Node Generators
// ============================================

fn arb_transform_type() -> impl Strategy<Value = TransformType> {
    prop_oneof![
        Just(TransformType::Jsonpath),
        Just(TransformType::Jmespath),
        Just(TransformType::Template),
        Just(TransformType::Javascript),
    ]
}

fn arb_transform_node_config() -> impl Strategy<Value = TransformNodeConfig> {
    (arb_standard_properties(), arb_transform_type()).prop_map(|(standard, transform_type)| {
        let expression = match transform_type {
            TransformType::Jsonpath => "$.data".to_string(),
            TransformType::Jmespath => "data".to_string(),
            TransformType::Template => "Hello {{name}}".to_string(),
            TransformType::Javascript => "return input.data;".to_string(),
        };
        TransformNodeConfig {
            standard,
            transform_type,
            expression,
            operations: None,
            type_coercion: None,
        }
    })
}

// ============================================
// Switch Node Generators
// ============================================

fn arb_evaluation_mode() -> impl Strategy<Value = EvaluationMode> {
    prop_oneof![Just(EvaluationMode::FirstMatch), Just(EvaluationMode::AllMatch),]
}

fn arb_switch_node_config() -> impl Strategy<Value = SwitchNodeConfig> {
    (arb_standard_properties(), arb_evaluation_mode()).prop_map(|(standard, evaluation_mode)| {
        SwitchNodeConfig {
            standard,
            evaluation_mode,
            conditions: vec![SwitchCondition {
                id: "cond_1".to_string(),
                name: "High".to_string(),
                field: "score".to_string(),
                operator: "gt".to_string(),
                value: Some(serde_json::json!(80)),
                output_port: "high".to_string(),
            }],
            default_branch: Some("default".to_string()),
            expression_mode: None,
        }
    })
}

// ============================================
// Loop Node Generators
// ============================================

fn arb_loop_type() -> impl Strategy<Value = LoopType> {
    prop_oneof![Just(LoopType::ForEach), Just(LoopType::While), Just(LoopType::Times),]
}

fn arb_loop_node_config() -> impl Strategy<Value = LoopNodeConfig> {
    (arb_standard_properties(), arb_loop_type()).prop_map(|(standard, loop_type)| {
        let for_each = if loop_type == LoopType::ForEach {
            Some(ForEachConfig {
                source_array: "items".to_string(),
                item_var: "item".to_string(),
                index_var: "index".to_string(),
            })
        } else {
            None
        };
        let while_config = if loop_type == LoopType::While {
            Some(WhileConfig { condition: "count < 10".to_string() })
        } else {
            None
        };
        let times = if loop_type == LoopType::Times {
            Some(TimesConfig { count: serde_json::json!(5) })
        } else {
            None
        };
        LoopNodeConfig {
            standard,
            loop_type,
            for_each,
            while_config,
            times,
            parallel: ParallelConfig::default(),
            results: ResultsConfig::default(),
        }
    })
}

// ============================================
// Merge Node Generators
// ============================================

fn arb_merge_mode() -> impl Strategy<Value = MergeMode> {
    prop_oneof![Just(MergeMode::WaitAll), Just(MergeMode::WaitAny), Just(MergeMode::WaitN),]
}

fn arb_combine_strategy() -> impl Strategy<Value = CombineStrategy> {
    prop_oneof![
        Just(CombineStrategy::Array),
        Just(CombineStrategy::Object),
        Just(CombineStrategy::First),
        Just(CombineStrategy::Last),
    ]
}

fn arb_merge_node_config() -> impl Strategy<Value = MergeNodeConfig> {
    (arb_standard_properties(), arb_merge_mode(), arb_combine_strategy()).prop_map(
        |(standard, mode, combine_strategy)| MergeNodeConfig {
            standard,
            mode,
            wait_count: Some(2),
            combine_strategy,
            branch_keys: None,
            timeout: MergeTimeout::default(),
        },
    )
}

// ============================================
// Wait Node Generators
// ============================================

fn arb_wait_type() -> impl Strategy<Value = WaitType> {
    prop_oneof![
        Just(WaitType::Fixed),
        Just(WaitType::Until),
        Just(WaitType::Webhook),
        Just(WaitType::Condition),
    ]
}

fn arb_wait_node_config() -> impl Strategy<Value = WaitNodeConfig> {
    (arb_standard_properties(), arb_wait_type()).prop_map(|(standard, wait_type)| {
        let fixed = if wait_type == WaitType::Fixed {
            Some(FixedDuration { duration: 1000, unit: "ms".to_string() })
        } else {
            None
        };
        let until = if wait_type == WaitType::Until {
            Some(UntilConfig { timestamp: "2025-01-01T00:00:00Z".to_string() })
        } else {
            None
        };
        let webhook = if wait_type == WaitType::Webhook {
            Some(WebhookWaitConfig { path: "/wait".to_string(), timeout: 30000 })
        } else {
            None
        };
        let condition = if wait_type == WaitType::Condition {
            Some(ConditionPolling {
                expression: "status == 'ready'".to_string(),
                poll_interval: 1000,
                max_wait: 60000,
            })
        } else {
            None
        };
        WaitNodeConfig { standard, wait_type, fixed, until, webhook, condition }
    })
}

// ============================================
// Code Node Generators
// ============================================

fn arb_code_language() -> impl Strategy<Value = CodeLanguage> {
    prop_oneof![Just(CodeLanguage::Javascript), Just(CodeLanguage::Typescript),]
}

fn arb_code_node_config() -> impl Strategy<Value = CodeNodeConfig> {
    (arb_standard_properties(), arb_code_language()).prop_map(|(standard, language)| {
        CodeNodeConfig {
            standard,
            language,
            code: "return input.data;".to_string(),
            sandbox: SandboxConfig::default(),
            input_type: None,
            output_type: None,
        }
    })
}

// ============================================
// Database Node Generators
// ============================================

fn arb_database_type() -> impl Strategy<Value = DatabaseType> {
    prop_oneof![
        Just(DatabaseType::Postgresql),
        Just(DatabaseType::Mysql),
        Just(DatabaseType::Sqlite),
        Just(DatabaseType::Mongodb),
        Just(DatabaseType::Redis),
    ]
}

fn arb_database_node_config() -> impl Strategy<Value = DatabaseNodeConfig> {
    (arb_standard_properties(), arb_database_type()).prop_map(|(standard, db_type)| {
        let sql = if matches!(
            db_type,
            DatabaseType::Postgresql | DatabaseType::Mysql | DatabaseType::Sqlite
        ) {
            Some(SqlConfig {
                operation: "query".to_string(),
                query: "SELECT * FROM users".to_string(),
                params: None,
            })
        } else {
            None
        };
        let mongodb = if db_type == DatabaseType::Mongodb {
            Some(MongoConfig {
                collection: "users".to_string(),
                operation: "find".to_string(),
                filter: Some(serde_json::json!({})),
                document: None,
            })
        } else {
            None
        };
        let redis = if db_type == DatabaseType::Redis {
            Some(RedisConfig {
                operation: "get".to_string(),
                key: "user:1".to_string(),
                value: None,
                ttl: None,
            })
        } else {
            None
        };
        DatabaseNodeConfig {
            standard,
            db_type,
            connection: DatabaseConnection {
                connection_string: "postgres://localhost/test".to_string(),
                credential_ref: None,
                pool_size: Some(5),
            },
            sql,
            mongodb,
            redis,
        }
    })
}

// ============================================
// Union Type Generator
// ============================================

fn arb_action_node_config() -> impl Strategy<Value = ActionNodeConfig> {
    prop_oneof![
        arb_trigger_node_config().prop_map(ActionNodeConfig::Trigger),
        arb_http_node_config().prop_map(ActionNodeConfig::Http),
        arb_set_node_config().prop_map(ActionNodeConfig::Set),
        arb_transform_node_config().prop_map(ActionNodeConfig::Transform),
        arb_switch_node_config().prop_map(ActionNodeConfig::Switch),
        arb_loop_node_config().prop_map(ActionNodeConfig::Loop),
        arb_merge_node_config().prop_map(ActionNodeConfig::Merge),
        arb_wait_node_config().prop_map(ActionNodeConfig::Wait),
        arb_code_node_config().prop_map(ActionNodeConfig::Code),
        arb_database_node_config().prop_map(ActionNodeConfig::Database),
    ]
}

// ============================================
// Property Tests
// ============================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: action-nodes, Property 10: Code Generation Validity**
    /// *For any* valid action node configuration, the generated code SHALL pass validation.
    /// **Validates: Requirements 13.1**
    #[test]
    fn prop_code_generation_validity(config in arb_action_node_config()) {
        let node_id = config.standard().id.clone();

        // Create a HashMap with the single config
        let mut nodes = HashMap::new();
        nodes.insert(node_id.clone(), config);

        let code = generate_action_nodes_code(&nodes);

        // Property: Generated code must pass validation
        let validation_result = validate_generated_code(&code);
        prop_assert!(
            validation_result.is_ok(),
            "Generated code failed validation for node '{}': {:?}\nGenerated code:\n{}",
            node_id,
            validation_result.err(),
            code
        );
    }

    /// **Feature: action-nodes, Property 10.1: Trigger Node Code Generation**
    /// *For any* valid trigger node configuration, the generated code SHALL be valid.
    /// **Validates: Requirements 13.1**
    #[test]
    fn prop_trigger_node_code_generation(config in arb_trigger_node_config()) {
        let code = config.generate_code(&config.standard.id);

        // Property: Generated code must contain the function definition
        prop_assert!(
            code.contains(&format!("async fn {}_trigger", config.standard.id)),
            "Generated code missing trigger function"
        );

        // Property: Generated code must pass validation
        let validation_result = validate_generated_code(&code);
        prop_assert!(
            validation_result.is_ok(),
            "Trigger node code failed validation: {:?}",
            validation_result.err()
        );
    }

    /// **Feature: action-nodes, Property 10.2: HTTP Node Code Generation**
    /// *For any* valid HTTP node configuration, the generated code SHALL be valid.
    /// **Validates: Requirements 13.1**
    #[test]
    fn prop_http_node_code_generation(config in arb_http_node_config()) {
        let code = config.generate_code(&config.standard.id);

        // Property: Generated code must contain the function definition
        prop_assert!(
            code.contains(&format!("async fn {}_http", config.standard.id)),
            "Generated code missing HTTP function"
        );

        // Property: Generated code must contain the URL
        prop_assert!(
            code.contains(&config.url) || code.contains("interpolate_variables"),
            "Generated code missing URL or interpolation"
        );

        // Property: Generated code must pass validation
        let validation_result = validate_generated_code(&code);
        prop_assert!(
            validation_result.is_ok(),
            "HTTP node code failed validation: {:?}",
            validation_result.err()
        );
    }

    /// **Feature: action-nodes, Property 10.3: Switch Node Code Generation**
    /// *For any* valid switch node configuration, the generated code SHALL be valid.
    /// **Validates: Requirements 13.1**
    #[test]
    fn prop_switch_node_code_generation(config in arb_switch_node_config()) {
        let code = config.generate_code(&config.standard.id);

        // Property: Generated code must contain the function definition
        prop_assert!(
            code.contains(&format!("async fn {}_switch", config.standard.id)),
            "Generated code missing switch function"
        );

        // Property: Generated code must pass validation
        let validation_result = validate_generated_code(&code);
        prop_assert!(
            validation_result.is_ok(),
            "Switch node code failed validation: {:?}",
            validation_result.err()
        );
    }

    /// **Feature: action-nodes, Property 10.4: All Node Types Generate Valid Code**
    /// *For any* action node type, the generated code SHALL have balanced syntax.
    /// **Validates: Requirements 13.1**
    #[test]
    fn prop_all_node_types_balanced_syntax(config in arb_action_node_config()) {
        let node_id = config.standard().id.clone();
        let code = match &config {
            ActionNodeConfig::Trigger(c) => c.generate_code(&node_id),
            ActionNodeConfig::Http(c) => c.generate_code(&node_id),
            ActionNodeConfig::Set(c) => c.generate_code(&node_id),
            ActionNodeConfig::Transform(c) => c.generate_code(&node_id),
            ActionNodeConfig::Switch(c) => c.generate_code(&node_id),
            ActionNodeConfig::Loop(c) => c.generate_code(&node_id),
            ActionNodeConfig::Merge(c) => c.generate_code(&node_id),
            ActionNodeConfig::Wait(c) => c.generate_code(&node_id),
            ActionNodeConfig::Code(c) => c.generate_code(&node_id),
            ActionNodeConfig::Database(c) => c.generate_code(&node_id),
            ActionNodeConfig::Email(_)
            | ActionNodeConfig::Notification(_)
            | ActionNodeConfig::Rss(_)
            | ActionNodeConfig::File(_) => String::new(),
        };

        // Property: Braces must be balanced
        let open_braces = code.matches('{').count();
        let close_braces = code.matches('}').count();
        prop_assert_eq!(
            open_braces, close_braces,
            "Unbalanced braces in generated code for node type '{}'",
            config.node_type()
        );

        // Property: Parentheses must be balanced
        let open_parens = code.matches('(').count();
        let close_parens = code.matches(')').count();
        prop_assert_eq!(
            open_parens, close_parens,
            "Unbalanced parentheses in generated code for node type '{}'",
            config.node_type()
        );
    }
}
