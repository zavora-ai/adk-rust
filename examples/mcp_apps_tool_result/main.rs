use adk_server::ui_types::{
    McpUiBridgeSnapshot, default_mcp_ui_host_capabilities, default_mcp_ui_host_info,
};
use anyhow::Context;
use serde_json::{Value, json};

fn build_example_payload() -> Value {
    let bridge_snapshot = McpUiBridgeSnapshot::new(
        "2025-11-25",
        true,
        default_mcp_ui_host_info(),
        default_mcp_ui_host_capabilities(),
        json!({
            "appName": "examples",
            "userId": "demo-user",
            "sessionId": "session-mcp-apps-example",
            "theme": "light",
            "locale": "en-US",
            "platform": "adk-server",
            "displayMode": "inline",
            "availableDisplayModes": ["inline"]
        }),
    )
    .with_app_metadata(
        json!({
            "name": "ExecutiveDashboardExample",
            "version": "0.4.1"
        }),
        json!({
            "availableDisplayModes": ["inline"],
            "tools": {
                "listChanged": false
            }
        }),
    );

    let tool_result = bridge_snapshot.build_tool_result(
        Some(json!({
            "surface": {
                "id": "dashboard",
                "title": "Executive Dashboard",
                "cards": [
                    { "label": "Revenue", "value": "$128K" },
                    { "label": "Users", "value": "4,210" },
                    { "label": "Retention", "value": "92%" }
                ],
                "alert": {
                    "severity": "warning",
                    "title": "Scheduled Maintenance",
                    "message": "Maintenance starts at 23:00 UTC."
                }
            }
        })),
        Some("ui://examples/executive-dashboard".to_string()),
        Some(
            r#"<!doctype html>
<html>
  <body>
    <main>
      <h1>Executive Dashboard</h1>
      <p>Fallback HTML for compatibility-oriented MCP Apps hosts.</p>
    </main>
  </body>
</html>"#
                .to_string(),
        ),
    );

    let payload = json!({
        "protocol": "mcp_apps",
        "toolResult": tool_result
    });

    payload
}

fn main() -> anyhow::Result<()> {
    let payload = build_example_payload();

    println!(
        "{}",
        serde_json::to_string_pretty(&payload)
            .context("failed to serialize canonical MCP Apps tool-result example")?
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::build_example_payload;
    use serde_json::Value;

    #[test]
    fn example_emits_canonical_mcp_apps_tool_result_shape() {
        let payload = build_example_payload();
        let expected: Value =
            serde_json::from_str(include_str!("expected.json")).expect("valid expected fixture");

        assert_eq!(payload, expected);

        assert_eq!(payload["protocol"], "mcp_apps");
        assert_eq!(payload["toolResult"]["resourceUri"], "ui://examples/executive-dashboard");
        assert!(payload["toolResult"]["html"].as_str().unwrap().contains("Executive Dashboard"));
        assert_eq!(payload["toolResult"]["bridge"]["protocolVersion"], "2025-11-25");
        assert_eq!(
            payload["toolResult"]["bridge"]["structuredContent"]["surface"]["title"],
            "Executive Dashboard"
        );
        assert_eq!(payload["toolResult"]["bridge"]["hostInfo"]["name"], "adk-server");
        assert_eq!(payload["toolResult"]["bridge"]["appInfo"]["name"], "ExecutiveDashboardExample");
        assert_eq!(
            payload["toolResult"]["bridge"]["appCapabilities"]["availableDisplayModes"][0],
            "inline"
        );
        assert_eq!(
            payload["toolResult"]["bridge"]["hostContext"]["sessionId"],
            "session-mcp-apps-example"
        );
        assert_eq!(payload["toolResult"]["bridge"]["initialized"], true);
    }
}
