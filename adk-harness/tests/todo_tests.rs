//! Todo writes in one batch never lose an update, and the declaration, the stored list and
//! the response name `activeForm` alike (spec 3.1 §7.6; evaluation finding F-11, s13).

#![cfg(feature = "runner")]

use crate::common;

use adk_harness::{TodoList, TodoStatus};
use adk_model::ScriptedResponse;
use serde_json::{Value, json};

async fn stored(f: &common::Fixture) -> TodoList {
    f.harness.todos(&f.user, &f.session).await.unwrap()
}

fn answers(events: &[adk_core::Event], tool: &str) -> Vec<Value> {
    common::responses(events).into_iter().filter(|(name, _)| name == tool).map(|(_, r)| r).collect()
}

#[tokio::test]
async fn two_updates_in_one_batch_both_land() {
    let script = vec![
        ScriptedResponse::function_call(
            "write_todos",
            json!({ "todos": [{ "content": "one" }, { "content": "two" }, { "content": "three" }] }),
        ),
        ScriptedResponse::function_calls([
            ("update_todo", json!({ "id": "1", "status": "completed" })),
            ("update_todo", json!({ "id": "2", "status": "in_progress" })),
        ]),
        ScriptedResponse::text("done"),
    ];
    let f = common::fixture(script, |b| b).await;
    let events = f.send("plan and start").await;

    let revisions: Vec<u64> = answers(&events, "update_todo")
        .iter()
        .map(|answer| answer["revision"].as_u64().unwrap())
        .collect();
    assert_eq!(revisions, [2, 3], "each update sees the one before it");
    let list = stored(&f).await;
    let statuses: Vec<TodoStatus> = list.items.iter().map(|item| item.status).collect();
    assert_eq!(statuses, [TodoStatus::Completed, TodoStatus::InProgress, TodoStatus::Pending]);
    assert_eq!(list.revision, 3);
}

#[tokio::test]
async fn active_form_is_named_alike_in_the_declaration_the_store_and_the_response() {
    let script = vec![
        ScriptedResponse::function_call(
            "write_todos",
            json!({ "todos": [
                { "content": "Run the tests", "activeForm": "Running the tests", "status": "in_progress" },
                { "content": "Fix the bug", "active_form": "Fixing the bug" }
            ] }),
        ),
        ScriptedResponse::text("done"),
    ];
    let f = common::fixture(script, |b| b).await;
    let events = f.send("plan").await;

    let declared = f.model.last_request().unwrap().tools["write_todos"].clone();
    let item = &declared["parameters"]["properties"]["todos"]["items"]["properties"];
    assert!(item.get("activeForm").is_some(), "{declared}");
    assert!(item.get("active_form").is_none(), "{declared}");

    let list = stored(&f).await;
    assert_eq!(list.items[0].active_form.as_deref(), Some("Running the tests"));
    assert_eq!(list.items[1].active_form.as_deref(), Some("Fixing the bug"));
    let state = serde_json::to_value(&list).unwrap();
    assert_eq!(state["items"][0]["activeForm"], "Running the tests");

    let answer = &answers(&events, "write_todos")[0];
    assert_eq!(answer["todos"][0]["activeForm"], "Running the tests", "{answer}");
    assert_eq!(answer["todos"][1]["activeForm"], "Fixing the bug", "{answer}");
}
