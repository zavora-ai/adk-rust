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
- fixed OAuth 2.0 client-credentials token acquisition, refreshed before the
  token's `expires_in` and after a 401;
- tokens minted per request by a `CredentialBroker` for the agent's acting
  identity (`with_credential_broker`), attached only to the endpoint they were
  minted for;
- request timeouts; and
- one bounded session reinitialization after an expired-session response.

`OAuth2Config` is not the complete MCP authorization flow. It does not perform
protected-resource metadata discovery, authorization-server discovery, browser
authorization, PKCE, or resource-indicator negotiation. Use `rmcp`'s
authorization APIs or an identity component when the deployment requires that
flow.

The token URL must use `https`; plain `http` is accepted only for a loopback
host, so client credentials and issued tokens are not sent unencrypted. When the
token endpoint answers with an error, the client secret is replaced with
`[REDACTED]` in the error body, which is cut to 512 characters. Bound the token
request so a slow or unreachable authorization server cannot hang connection
setup:

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

An approval is bound to the call's fingerprint: the acting agent, the tool's
origin, the tool name, and the canonical arguments. An MCP tool's origin is
`mcp:{toolset name}` plus a SHA-256 digest of its declaration and annotations,
so a held call does not resume on another server that publishes the same tool
name, or on a tool the server redefined after the approval. Give each
`McpToolset` a distinct `with_name`; unnamed toolsets share the name
`mcp_toolset`. See
[Tool Authorization](../security/tool-authorization.md#decisions-authorize-one-exact-call)
and `examples/approval_scoping`.

Tool schemas and descriptions are server-controlled input. `McpToolset` skips a
tool whose input or output schema exceeds `McpSchemaLimits` (256 KiB and 10 000
JSON values by default) and logs the measured size, never the schema. It cuts a
description over 8 KiB. A schema whose definitions refer to themselves, or fan
out through a chain of references, is inlined at most once per definition on
each path and within a fixed budget, so normalizing it for a provider stays
linear in its size. See [Schema size limits](client.md#schema-size-limits).

For consequential tools:

- show the person the final resolved arguments;
- distinguish allow-once from durable policy;
- keep approval bound to the call's fingerprint, never to the tool name alone;
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

A failed MCP call — a transport error or a result with `isError: true` — is a
tool error. `LlmAgent` sends the model only its component, category, and code,
so server-supplied error text does not reach the model provider or the session
unless the agent sets `expose_tool_error_messages(true)`. See
[Error Handling](../tools/function-tools.md#error-handling).
