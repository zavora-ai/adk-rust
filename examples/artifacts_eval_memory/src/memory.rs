//! Memory scenarios: recall through punctuation, ranked results, and entries without words.

use std::sync::{Arc, Mutex};

use adk_core::{Content, Llm, Part, ToolContext};
use adk_memory::{
    InMemoryMemoryService, MemoryEntry, MemoryService, MemoryServiceAdapter, SearchRequest,
};
use chrono::{Duration, Utc};
use futures::FutureExt;
use serde_json::{Value, json};

use crate::common::{APP, Services, USER, Verdict, agent, brief, json_tool, run_turn};

fn entry(text: &str, minutes_ago: i64) -> MemoryEntry {
    MemoryEntry {
        content: Content::new("user").with_text(text),
        author: "user".to_string(),
        timestamp: Utc::now() - Duration::minutes(minutes_ago),
    }
}

/// Each `recall` call: the query and the notes it returned.
type RecallLog = Arc<Mutex<Vec<(String, Vec<String>)>>>;

fn first_text(entry: &MemoryEntry) -> String {
    adk_memory::text::extract_text(&entry.content)
}

/// SS20: facts stored with punctuation are found by the words they contain.
pub async fn punctuation_recall(model: Arc<dyn Llm>) -> anyhow::Result<Verdict> {
    let service = Arc::new(InMemoryMemoryService::new());
    service
        .add_session(
            APP,
            USER,
            "earlier-session",
            vec![
                entry("Hometown: Nairobi.", 30),
                entry("Favourite colour: teal!", 20),
                entry("Pet: a dog called Kibo.", 10),
            ],
        )
        .await?;
    let calls: RecallLog = Arc::default();
    let recall = {
        let calls = calls.clone();
        json_tool(
            "recall",
            "Searches the user's long-term memory by keywords and returns matching notes.",
            json!({
                "type": "object",
                "properties": {"query": {"type": "string", "description": "Keywords to look up"}},
                "required": ["query"]
            }),
            move |ctx: Arc<dyn ToolContext>, args: Value| {
                let calls = calls.clone();
                async move {
                    let query = args["query"].as_str().unwrap_or_default().to_string();
                    let found: Vec<String> = ctx
                        .search_memory(&query)
                        .await?
                        .iter()
                        .map(|memory| adk_memory::text::extract_text(&memory.content))
                        .collect();
                    calls.lock().unwrap().push((query, found.clone()));
                    Ok(json!({ "notes": found }))
                }
                .boxed()
            },
        )
    };
    let agent = agent(
        model,
        "You answer questions about the user from long-term memory. Always look facts up with \
         the recall tool, using short keyword queries, before you answer.",
        vec![recall],
    )?;
    let answer = run_turn(
        agent,
        Services {
            artifacts: None,
            memory: Some(Arc::new(MemoryServiceAdapter::new(service, APP, USER))),
        },
        "s-recall",
        "What is my hometown? Look it up with the recall tool, then answer in one sentence.",
    )
    .await?;
    let calls = calls.lock().unwrap().clone();
    if calls.is_empty() {
        return Ok(Verdict::Skip(format!("the model did not call recall: {}", brief(&answer))));
    }
    let found = calls.iter().any(|(_, notes)| notes.iter().any(|note| note.contains("Nairobi")));
    let evidence = format!("recall calls {calls:?}; model: {}", brief(&answer));
    Ok(if found && answer.contains("Nairobi") {
        Verdict::Pass(evidence)
    } else {
        Verdict::Fail(evidence)
    })
}

/// SS20: the entry matching the most query terms comes first.
pub async fn ranked_top_k() -> anyhow::Result<Verdict> {
    let instances = 20;
    let mut first_hits = 0;
    let mut sample = String::new();
    for _ in 0..instances {
        let service = InMemoryMemoryService::new();
        for index in 0..30 {
            service
                .add_session(
                    APP,
                    USER,
                    &format!("decoy-{index}"),
                    vec![entry(&format!("Rust note number {index}."), index)],
                )
                .await?;
        }
        service
            .add_session(
                APP,
                USER,
                "target",
                vec![entry("Rust ownership and borrowing, explained.", 90)],
            )
            .await?;
        let found = service
            .search(SearchRequest {
                query: "rust ownership borrowing".to_string(),
                user_id: USER.to_string(),
                app_name: APP.to_string(),
                limit: Some(3),
                min_score: None,
                project_id: None,
            })
            .await?
            .memories;
        let texts: Vec<String> = found.iter().map(first_text).collect();
        if texts.first().is_some_and(|text| text.contains("ownership")) {
            first_hits += 1;
        }
        sample = format!("{texts:?}");
    }
    let evidence = format!(
        "the 3-term match ranked first in {first_hits}/{instances} stores of 31 \"rust\" entries; \
         last top-3: {sample}"
    );
    Ok(if first_hits == instances { Verdict::Pass(evidence) } else { Verdict::Fail(evidence) })
}

/// SS20: an entry with no words (an image) is stored and listed.
pub async fn image_only_entry() -> anyhow::Result<Verdict> {
    let service = InMemoryMemoryService::new();
    let image = MemoryEntry {
        content: Content {
            role: "user".to_string(),
            parts: vec![Part::InlineData {
                mime_type: "image/png".to_string(),
                data: vec![0x89, b'P', b'N', b'G'],
                uri: None,
                annotations: None,
            }],
        },
        author: "user".to_string(),
        timestamp: Utc::now(),
    };
    service
        .add_session(APP, USER, "s-image", vec![image, entry("Whiteboard photo from the call", 1)])
        .await?;
    let recent = service.list_recent(APP, USER, 10).await?;
    let kinds: Vec<&str> = recent
        .iter()
        .map(|memory| match memory.content.parts.first() {
            Some(Part::InlineData { .. }) => "image",
            Some(_) => "text",
            None => "empty",
        })
        .collect();
    let evidence = format!("list_recent returns {} entries: {kinds:?}", recent.len());
    Ok(if kinds.contains(&"image") && kinds.contains(&"text") {
        Verdict::Pass(evidence)
    } else {
        Verdict::Fail(evidence)
    })
}
