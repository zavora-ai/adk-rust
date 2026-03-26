//! File Node scenarios: write, read (text + JSON + CSV), list, delete.

use adk_action::*;
use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::state::State;
use adk_graph::ExecutionConfig;
use anyhow::Result;
use serde_json::json;

use super::set_node::standard;

pub async fn run() -> Result<()> {
    println!("── 7. File Node ───────────────────────────────");

    let tmp_dir = std::env::temp_dir().join("adk_action_example");
    let _ = tokio::fs::create_dir_all(&tmp_dir).await;

    // 7a. Write a JSON file
    let json_path = tmp_dir.join("data.json").to_string_lossy().to_string();
    let graph = GraphAgent::builder("file-write-json")
        .description("Write JSON file")
        .channels(&["writeResult"])
        .action_node(ActionNodeConfig::File(FileNodeConfig {
            standard: standard("write_json", "Write JSON", "writeResult"),
            operation: FileOperation::Write,
            local: Some(LocalFileConfig { path: json_path.clone() }),
            cloud: None,
            parse: None,
            write: Some(FileWriteConfig {
                content: json!({"users": [{"name": "Alice"}, {"name": "Bob"}]}),
                create_dirs: true,
                append: false,
            }),
            list: None,
        }))
        .edge(START, "write_json")
        .edge("write_json", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    println!("  write JSON:  written={}", result["writeResult"]["written"]);
    assert_eq!(result["writeResult"]["written"], json!(true));

    // 7b. Read the JSON file back
    let graph = GraphAgent::builder("file-read-json")
        .description("Read JSON file")
        .channels(&["readResult"])
        .action_node(ActionNodeConfig::File(FileNodeConfig {
            standard: standard("read_json", "Read JSON", "readResult"),
            operation: FileOperation::Read,
            local: Some(LocalFileConfig { path: json_path.clone() }),
            cloud: None,
            parse: Some(FileParseConfig {
                format: FileFormat::Json,
                csv_options: None,
            }),
            write: None,
            list: None,
        }))
        .edge(START, "read_json")
        .edge("read_json", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    let data = &result["readResult"];
    println!("  read JSON:   users={}", data["users"]);
    assert_eq!(data["users"][0]["name"], json!("Alice"));

    // 7c. Write and read a CSV file
    let csv_path = tmp_dir.join("data.csv").to_string_lossy().to_string();
    let graph = GraphAgent::builder("file-write-csv")
        .description("Write CSV file")
        .channels(&["writeResult"])
        .action_node(ActionNodeConfig::File(FileNodeConfig {
            standard: standard("write_csv", "Write CSV", "writeResult"),
            operation: FileOperation::Write,
            local: Some(LocalFileConfig { path: csv_path.clone() }),
            cloud: None,
            parse: None,
            write: Some(FileWriteConfig {
                content: json!("name,age,city\nAlice,30,NYC\nBob,25,LA\nCharlie,35,SF"),
                create_dirs: true,
                append: false,
            }),
            list: None,
        }))
        .edge(START, "write_csv")
        .edge("write_csv", END)
        .build()?;
    graph.invoke(State::new(), ExecutionConfig::new("test")).await?;

    let graph = GraphAgent::builder("file-read-csv")
        .description("Read CSV file")
        .channels(&["readResult"])
        .action_node(ActionNodeConfig::File(FileNodeConfig {
            standard: standard("read_csv", "Read CSV", "readResult"),
            operation: FileOperation::Read,
            local: Some(LocalFileConfig { path: csv_path.clone() }),
            cloud: None,
            parse: Some(FileParseConfig {
                format: FileFormat::Csv,
                csv_options: Some(CsvOptions {
                    delimiter: ",".into(),
                    has_header: true,
                }),
            }),
            write: None,
            list: None,
        }))
        .edge(START, "read_csv")
        .edge("read_csv", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    let rows = result["readResult"].as_array().unwrap();
    println!("  read CSV:    {} rows, first={}", rows.len(), rows[0]);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["name"], json!("Alice"));

    // 7d. List directory
    let graph = GraphAgent::builder("file-list")
        .description("List directory")
        .channels(&["listResult"])
        .action_node(ActionNodeConfig::File(FileNodeConfig {
            standard: standard("list_dir", "List Files", "listResult"),
            operation: FileOperation::List,
            local: Some(LocalFileConfig { path: tmp_dir.to_string_lossy().to_string() }),
            cloud: None,
            parse: None,
            write: None,
            list: Some(FileListConfig {
                recursive: false,
                pattern: Some("*.json".into()),
            }),
        }))
        .edge(START, "list_dir")
        .edge("list_dir", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    let files = result["listResult"].as_array().unwrap();
    println!("  list(*.json): {} files", files.len());
    assert!(files.len() >= 1);

    // 7e. Delete files and cleanup
    let graph = GraphAgent::builder("file-delete")
        .description("Delete file")
        .channels(&["deleteResult"])
        .action_node(ActionNodeConfig::File(FileNodeConfig {
            standard: standard("delete_file", "Delete JSON", "deleteResult"),
            operation: FileOperation::Delete,
            local: Some(LocalFileConfig { path: json_path }),
            cloud: None,
            parse: None,
            write: None,
            list: None,
        }))
        .edge(START, "delete_file")
        .edge("delete_file", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    println!("  delete:      deleted={}", result["deleteResult"]["deleted"]);
    assert_eq!(result["deleteResult"]["deleted"], json!(true));

    // Cleanup remaining files
    let _ = tokio::fs::remove_file(csv_path).await;
    let _ = tokio::fs::remove_dir(&tmp_dir).await;

    println!("  ✓ All File node scenarios passed\n");
    Ok(())
}
