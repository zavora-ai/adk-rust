#![cfg(feature = "yaml")]
//! YAML and JSON workflow definitions: parsing, validation with locations, building against a
//! registry, conditional routing, interrupts that resume, round trips, and the committed JSON
//! Schema. Agent nodes run on a scripted model; the live run is in
//! `workflow_definition_live_tests`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use adk_core::{Content, Llm, LlmRequest, LlmResponse, LlmResponseStream};
use adk_graph::definition::{
    SourceLocation, WorkflowDefinition, WorkflowError, WorkflowIssue, WorkflowRegistry,
    workflow_json_schema,
};
use adk_graph::{ExecutionConfig, GraphError, Interrupt, NodeOutput, State};
use async_trait::async_trait;
use proptest::prelude::*;
use serde_json::{Value, json};

const MODEL: &str = "anthropic/claude-haiku-5-5";

fn sample_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/workflows/refund_desk.yaml")
}

/// Answers each agent of the sample workflow by its instruction.
struct Scripted {
    calls: Mutex<Vec<String>>,
}

impl Scripted {
    fn new() -> Arc<Self> {
        Arc::new(Self { calls: Mutex::new(Vec::new()) })
    }
}

#[async_trait]
impl Llm for Scripted {
    fn name(&self) -> &str {
        "claude-haiku-5-5"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let seen = format!("{request:?}");
        let message: String = request
            .contents
            .iter()
            .rev()
            .find(|content| content.role == "user")
            .map(|content| content.parts.iter().filter_map(adk_core::Part::text).collect())
            .unwrap_or_default();
        let reply = if seen.contains("Classify the customer") {
            self.calls.lock().unwrap().push("classify".into());
            if message.contains("money back") { "refund" } else { "question" }
        } else if seen.contains("refund is approved") {
            self.calls.lock().unwrap().push("refund_reply".into());
            "Your refund is approved."
        } else if seen.contains("declined") {
            self.calls.lock().unwrap().push("decline_reply".into());
            "Your refund was declined."
        } else {
            self.calls.lock().unwrap().push("answer".into());
            "We open at nine."
        };
        let response = LlmResponse {
            content: Some(Content::new("model").with_text(reply)),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter(vec![Ok(response)])))
    }
}

fn input(text: &str) -> State {
    State::from([("input".to_string(), json!(text))])
}

fn issue(path: &str, line: u32, column: u32, message: &str) -> WorkflowIssue {
    WorkflowIssue::new(path, Some(SourceLocation { line, column }), message)
}

#[tokio::test]
async fn a_refund_pauses_for_approval_and_resumes_to_the_refund_reply() {
    let model = Scripted::new();
    let registry = WorkflowRegistry::new().model(MODEL, model.clone() as Arc<dyn Llm>);
    let graph =
        WorkflowDefinition::from_file(sample_path()).unwrap().build(&registry).await.unwrap();

    let paused = graph
        .invoke(input("I want my money back for order 42"), ExecutionConfig::new("t1"))
        .await
        .unwrap_err();
    let GraphError::Interrupted(paused) = paused else {
        panic!("expected a pause, got {paused:?}")
    };
    let Interrupt::Dynamic { message, response_schema, .. } = &paused.interrupt else {
        panic!("expected the approval pause, got {:?}", paused.interrupt)
    };
    assert_eq!(message, "Refund requested: I want my money back for order 42 -- approve it?");
    assert!(response_schema.is_some());
    assert_eq!(paused.state["category"], json!("refund"));

    let state = graph
        .invoke(
            State::new(),
            ExecutionConfig::new("t1").with_resume_value("approve", json!({ "approved": true })),
        )
        .await
        .unwrap();
    assert_eq!(state["approval"], json!({ "approved": true }));
    assert_eq!(state["reply"], json!("Your refund is approved."));
    assert_eq!(state["status"], json!("closed: refund"));
    assert_eq!(*model.calls.lock().unwrap(), ["classify", "refund_reply"]);
}

#[tokio::test]
async fn a_declined_refund_takes_the_default_route() {
    let model = Scripted::new();
    let registry = WorkflowRegistry::new().model(MODEL, model.clone() as Arc<dyn Llm>);
    let graph =
        WorkflowDefinition::from_file(sample_path()).unwrap().build(&registry).await.unwrap();

    let _ = graph.invoke(input("money back please"), ExecutionConfig::new("t2")).await.unwrap_err();
    let state = graph
        .invoke(
            State::new(),
            ExecutionConfig::new("t2").with_resume_value("approve", json!({ "approved": false })),
        )
        .await
        .unwrap();
    assert_eq!(state["reply"], json!("Your refund was declined."));
    assert_eq!(*model.calls.lock().unwrap(), ["classify", "decline_reply"]);
}

#[tokio::test]
async fn a_question_does_not_pause() {
    let model = Scripted::new();
    let registry = WorkflowRegistry::new().model(MODEL, model.clone() as Arc<dyn Llm>);
    let graph =
        WorkflowDefinition::from_file(sample_path()).unwrap().build(&registry).await.unwrap();

    let state = graph.invoke(input("When do you open?"), ExecutionConfig::new("t3")).await.unwrap();
    assert_eq!(state["reply"], json!("We open at nine."));
    assert_eq!(state["status"], json!("closed: question"));
    assert_eq!(*model.calls.lock().unwrap(), ["classify", "answer"]);
}

#[tokio::test]
async fn an_answer_outside_the_response_schema_is_refused() {
    let registry = WorkflowRegistry::new().model(MODEL, Scripted::new() as Arc<dyn Llm>);
    let graph =
        WorkflowDefinition::from_file(sample_path()).unwrap().build(&registry).await.unwrap();

    let _ = graph.invoke(input("money back"), ExecutionConfig::new("t4")).await.unwrap_err();
    let refused = graph
        .invoke(
            State::new(),
            ExecutionConfig::new("t4").with_resume_value("approve", json!({ "approved": "yes" })),
        )
        .await;
    assert!(matches!(refused, Err(GraphError::ResumeRejected { .. })), "{refused:?}");
}

#[tokio::test]
async fn registered_agents_functions_and_switch_cases_build_and_route() {
    let echo = adk_agent::LlmAgentBuilder::new("echo")
        .model(Scripted::new() as Arc<dyn Llm>)
        .instruction("Answer the customer's question.")
        .build()
        .unwrap();
    let registry =
        WorkflowRegistry::new().agent(Arc::new(echo)).function("score", |ctx| async move {
            let words = ctx.state["input"].as_str().unwrap_or_default().split_whitespace().count();
            Ok(NodeOutput::new().with_update("words", json!(words)))
        });
    let workflow = WorkflowDefinition::from_yaml_str(
        r#"
version: 1
name: sizes
nodes:
  score: { function: score }
  long: { agent: echo, output_key: said }
  short: { set: { size: short } }
  tag: { set: { size: "{{words}} words" } }
edges:
  - [START, score]
  - from: score
    routes:
      - when:
          all:
            - { field: words, op: gte, value: 3 }
            - not: { field: input, op: contains, value: skip }
        to: long
    default: short
  - from: long
    switch: said
    cases:
      "We open at nine.": tag
  - [short, END]
  - [tag, END]
"#,
    )
    .unwrap();
    let graph = workflow.build(&registry).await.unwrap();

    let long = graph.invoke(input("when do you open"), ExecutionConfig::new("a")).await.unwrap();
    assert_eq!(long["said"], json!("We open at nine."));
    assert_eq!(long["size"], json!("4 words"));
    let short = graph.invoke(input("hi"), ExecutionConfig::new("b")).await.unwrap();
    assert_eq!(short["size"], json!("short"));
    let skipped =
        graph.invoke(input("skip this one please"), ExecutionConfig::new("c")).await.unwrap();
    assert_eq!(skipped["size"], json!("short"));
}

#[tokio::test]
async fn an_action_node_runs_with_defaults_from_its_node() {
    let workflow = WorkflowDefinition::from_yaml_str(
        r#"
version: 1
name: greet
nodes:
  greet:
    action: { type: set, mode: set, variables: [{ key: greeting, value: "Hello {{name}}" }] }
edges:
  - [START, greet, END]
"#,
    )
    .unwrap();
    let graph = workflow.build(&WorkflowRegistry::new()).await.unwrap();
    let state = graph
        .invoke(State::from([("name".to_string(), json!("Ada"))]), ExecutionConfig::new("g"))
        .await
        .unwrap();
    assert_eq!(state["greeting"], json!("Hello Ada"));
}

#[tokio::test]
async fn interrupt_before_pauses_until_the_thread_resumes() {
    let workflow = WorkflowDefinition::from_yaml_str(
        r#"
version: 1
name: gate
state:
  count: { reducer: sum, default: 0 }
nodes:
  first: { set: { count: 1 } }
  second: { set: { count: 2 } }
edges:
  - [START, first, second, END]
interrupt_before: [second]
recursion_limit: 10
"#,
    )
    .unwrap();
    let graph = workflow.build(&WorkflowRegistry::new()).await.unwrap();
    let paused = graph.invoke(State::new(), ExecutionConfig::new("gate")).await.unwrap_err();
    assert!(
        matches!(&paused, GraphError::Interrupted(paused) if matches!(&paused.interrupt, Interrupt::Before(node) if node == "second")),
        "{paused:?}"
    );
    let state = graph.invoke(State::new(), ExecutionConfig::new("gate")).await.unwrap();
    assert_eq!(state["count"], json!(3.0));
}

#[tokio::test]
async fn a_workflow_builds_into_a_graph_agent_named_after_it() {
    let registry = WorkflowRegistry::new().model(MODEL, Scripted::new() as Arc<dyn Llm>);
    let agent =
        WorkflowDefinition::from_file(sample_path()).unwrap().build_agent(&registry).await.unwrap();
    assert_eq!(adk_core::Agent::name(&agent), "refund_desk");
}

#[test]
fn every_structural_issue_is_reported_with_its_path_and_location() {
    let error = WorkflowDefinition::from_yaml_str(
        r#"version: 1
name: broken
nodes:
  a: { set: { x: 1 } }
  b:
    set: { y: 1 }
    function: f
  orphan: { set: { z: 1 } }
  ask: { interrupt: { message: "ok?" } }
edges:
  - [START, a, missing]
  - from: a
    routes:
      - when: { field: x, op: matches, value: "(" }
        to: b
  - from: START
    switch: x
    cases: { "1": a }
  - [b, ask, END]
checkpoint: { store: none }
"#,
    )
    .unwrap_err();
    assert_eq!(
        error.issues,
        vec![
            issue("nodes.b", 6, 5, "the node sets function and set; set exactly one kind"),
            issue(
                "nodes.orphan",
                8,
                11,
                "node 'orphan' is unreachable from START; add an edge to it"
            ),
            issue(
                "edges[0][2]",
                11,
                16,
                "unknown node 'missing'; declared nodes are a, ask, b, orphan"
            ),
            issue(
                "edges[1].routes[0].when.value",
                14,
                47,
                "not a valid regular expression: regex parse error:\n    (\n    ^\nerror: unclosed group"
            ),
            issue(
                "edges[2].from",
                16,
                11,
                "edges from START cannot be conditional; route from the first node instead"
            ),
            issue(
                "checkpoint.store",
                20,
                22,
                "a workflow that pauses needs checkpoints to resume from; use `memory` or `sqlite`"
            ),
        ]
    );
}

#[test]
fn a_type_error_is_located_by_the_parser() {
    let error = WorkflowDefinition::from_yaml_str(
        "version: 1\nname: w\nnodes:\n  a: { set: { x: 1 } }\nedges:\n  - from: a\n    routes:\n      - when: { field: x, op: approx, value: 1 }\n        to: END\n",
    )
    .unwrap_err();
    assert_eq!(error.issues.len(), 1);
    assert_eq!(error.issues[0].location, Some(SourceLocation { line: 8, column: 31 }));
    assert!(error.issues[0].message.contains("approx"), "{}", error.issues[0].message);

    let unknown = WorkflowDefinition::from_yaml_str(
        "version: 1\nname: w\nnodes:\n  a: { set: { x: 1 }, retries: 3 }\nedges: [[START, a]]\n",
    )
    .unwrap_err();
    assert_eq!(unknown.issues[0].location.map(|l| l.line), Some(4));
    assert!(unknown.issues[0].message.contains("retries"), "{}", unknown.issues[0].message);
}

#[test]
fn a_json_workflow_reports_paths_and_parser_locations() {
    let invalid = WorkflowDefinition::from_json_str(
        r#"{"version": 1, "name": "w", "nodes": {"a": {"set": {"x": 1}}}, "edges": [["START", "b"]]}"#,
    )
    .unwrap_err();
    assert_eq!(invalid.issues[0].path, "edges[0][1]");
    assert_eq!(invalid.issues[0].location, None);

    let malformed =
        WorkflowDefinition::from_json_str("{\"version\": 1,\n \"name\": }").unwrap_err();
    assert_eq!(malformed.issues[0].location.map(|l| l.line), Some(2));
}

#[tokio::test]
async fn names_the_registry_cannot_resolve_are_located_at_build() {
    let workflow = WorkflowDefinition::from_yaml_str(
        "version: 1\nname: w\nnodes:\n  a: { agent: ghost }\n  b:\n    llm: { model: openai/gpt-x, tools: [search] }\nedges:\n  - [START, a, b, END]\n",
    )
    .unwrap();
    let Err(error) = workflow.build(&WorkflowRegistry::new()).await else {
        panic!("the unregistered names build");
    };
    assert_eq!(
        error.issues,
        vec![
            issue(
                "nodes.a.agent",
                4,
                15,
                "no agent named 'ghost' is registered; registered agents: none"
            ),
            issue(
                "nodes.b.llm.model",
                6,
                19,
                "no model 'openai/gpt-x' is registered and the registry has no model resolver; \
                 register it with WorkflowRegistry::model or set a model_resolver"
            ),
        ]
    );
    let shown = error.to_string();
    assert!(shown.starts_with("4:15: nodes.a.agent: no agent named 'ghost'"), "{shown}");
}

#[test]
fn the_sample_round_trips_through_yaml_and_json() {
    let workflow = WorkflowDefinition::from_file(sample_path()).unwrap();
    let yaml = workflow.to_yaml_string().unwrap();
    assert_eq!(WorkflowDefinition::from_yaml_str(&yaml).unwrap(), workflow);
    let json = serde_json::to_string_pretty(&workflow).unwrap();
    assert_eq!(WorkflowDefinition::from_json_str(&json).unwrap(), workflow);
}

#[test]
fn the_committed_json_schema_matches_the_types() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas/workflow.schema.json");
    let text = serde_json::to_string_pretty(&workflow_json_schema()).unwrap() + "\n";
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
        panic!("{} was out of date and has been rewritten; commit it", path.display());
    }
}

#[test]
fn the_sample_satisfies_the_json_schema() {
    let schema = jsonschema::validator_for(&workflow_json_schema()).unwrap();
    let workflow = WorkflowDefinition::from_file(sample_path()).unwrap();
    let value = serde_json::to_value(&workflow).unwrap();
    let errors: Vec<String> = schema.iter_errors(&value).map(|error| error.to_string()).collect();
    assert!(errors.is_empty(), "{errors:?}");
    let invalid =
        json!({ "version": 1, "name": "w", "nodes": { "a": { "sett": {} } }, "edges": [] });
    assert!(!schema.is_valid(&invalid));
}

#[test]
fn errors_name_the_file() {
    let dir = tempfile_dir();
    let path = dir.join("bad.yaml");
    std::fs::write(
        &path,
        "version: 1\nname: w\nnodes:\n  a: { set: { x: 1 } }\nedges: [[START, b]]\n",
    )
    .unwrap();
    let error: WorkflowError = WorkflowDefinition::from_file(&path).unwrap_err();
    let file = path.display();
    assert_eq!(
        error.to_string(),
        format!(
            "{file}:4:6: nodes.a: node 'a' is unreachable from START; add an edge to it\n\
             {file}:5:17: edges[0][1]: unknown node 'b'; declared nodes are a"
        )
    );
    std::fs::remove_dir_all(dir).unwrap();
}

fn tempfile_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("adk-graph-workflow-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A random workflow: a chain of `set` nodes with a conditional skip over the middle.
fn workflows() -> impl Strategy<Value = String> {
    (
        prop::collection::btree_set("[a-z][a-z0-9_]{0,6}", 2..6),
        prop::sample::select(vec!["eq", "neq", "gt", "contains", "exists"]),
        -5i64..5,
    )
        .prop_map(|(ids, op, threshold)| {
            let ids: Vec<String> = ids.into_iter().map(|id| format!("n_{id}")).collect();
            let mut yaml = String::from("version: 1\nname: generated\nnodes:\n");
            for (index, id) in ids.iter().enumerate() {
                yaml.push_str(&format!("  {id}: {{ set: {{ step: {index}, last: \"{id}\" }} }}\n"));
            }
            yaml.push_str(&format!("edges:\n  - [START, {}]\n", ids[0]));
            let value = if op == "exists" { String::new() } else { format!(", value: {threshold}") };
            yaml.push_str(&format!(
                "  - from: {}\n    routes:\n      - when: {{ field: step, op: {op}{value} }}\n        to: {}\n    default: {}\n",
                ids[0],
                ids[ids.len() - 1],
                ids[1],
            ));
            for pair in ids[1..].windows(2) {
                yaml.push_str(&format!("  - [{}, {}]\n", pair[0], pair[1]));
            }
            yaml.push_str(&format!("  - [{}, END]\n", ids[ids.len() - 1]));
            yaml
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn generated_workflows_round_trip_and_end_at_the_last_node(yaml in workflows()) {
        let workflow = WorkflowDefinition::from_yaml_str(&yaml).unwrap();
        let again = WorkflowDefinition::from_yaml_str(&workflow.to_yaml_string().unwrap()).unwrap();
        prop_assert_eq!(&again, &workflow);
        let json = serde_json::to_string(&workflow).unwrap();
        prop_assert_eq!(&WorkflowDefinition::from_json_str(&json).unwrap(), &workflow);

        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let state = runtime.block_on(async {
            workflow.build(&WorkflowRegistry::new()).await.unwrap()
                .invoke(State::new(), ExecutionConfig::new("p")).await.unwrap()
        });
        let last = workflow.nodes.keys().last().unwrap();
        prop_assert_eq!(&state["last"], &Value::String(last.clone()));
    }
}
