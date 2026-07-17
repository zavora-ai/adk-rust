# MCP security and authorization

MCP standardizes how capabilities are described and called. It does not decide
which capabilities an agent should receive or which side effects a user has
approved.

## Four separate decisions

1. **Connection authentication** — may this client connect to this server?
2. **Capability visibility** — which tools and resources may the model see?
3. **Execution authorization** — may this identity perform this specific action
   on this resource now?
4. **Human approval** — does a consequential action require a person to confirm
   its exact inputs and effects?

Do not collapse these decisions into an `autoApprove` configuration list.

## Local stdio servers

A local child process inherits a powerful position inside the application host.

- Use an absolute, reviewed executable path.
- Pin package and binary versions; avoid `latest` tags.
- Pass only required environment variables.
- Do not place secrets in command-line arguments.
- Restrict filesystem roots and working directories.
- Apply an OS sandbox profile when the server handles untrusted input.
- Treat server descriptions, resources, and results as untrusted content.

The JSON-loading and runtime add/update paths validate server IDs.
`McpServerManager` does not sandbox the configured command.

## Remote Streamable HTTP

`McpHttpClientBuilder` can apply:

- bearer tokens;
- a caller-selected API-key header;
- arbitrary reviewed headers;
- fixed OAuth 2.0 client-credentials token acquisition;
- request timeouts; and
- one bounded session reinitialization after an expired-session response.

`OAuth2Config` is not the complete MCP authorization flow. It does not perform
protected-resource metadata discovery, authorization-server discovery, browser
authorization, PKCE, or resource-indicator negotiation. Use `rmcp`'s
authorization APIs or an identity component when the deployment requires that
flow.

Bound the token request so a slow or unreachable authorization server cannot
hang connection setup, and note that the client never echoes the client secret
back — token-endpoint error bodies are redacted before they reach logs:

```rust
use adk_tool::mcp::OAuth2Config;
use std::time::Duration;

let auth = OAuth2Config::new(client_id, token_url)
    .with_secret(client_secret)
    .with_scopes(vec!["mcp.read".into(), "mcp.invoke".into()])
    .with_timeout(Duration::from_secs(10)); // token request timeout
```

The default token-request timeout is 30 seconds.

## Tool exposure and execution

Use `with_tools` or `with_filter` to keep unnecessary capabilities out of the
model request. Then apply ADK-Rust tool authorization and confirmation at
execution time.

For consequential tools:

- show the person the final resolved arguments;
- distinguish allow-once from durable policy;
- keep approval bound to the exact function-call ID;
- make external writes idempotent where possible;
- store the approval decision and tool outcome together; and
- never treat a successful protocol response as proof of a successful business
  outcome without checking the returned evidence.

## Elicitation

Elicitation is a server request for more information, not an instruction the
application must obey. Review the message, URL, requested fields, and metadata.
Decline unsupported or unexpected requests. Validate all accepted form values
before using them.

## Logging and secrets

Redact environment variables, authorization headers, API keys, elicitation
answers, and sensitive tool arguments. Record server ID, tool name, task ID,
status, timing, approval, and a bounded result summary instead.
