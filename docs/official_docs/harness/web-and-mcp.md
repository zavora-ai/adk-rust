# Harness Web Access and MCP Interaction

The harness reads the web and answers MCP servers under the same governance as every
other tool call (spec 3.1 §7.20, §7.21). `web_fetch` and `web_search` are governed tools
whose class comes from the URL's host; everything they return reaches the model inside a
delimited untrusted block, screened before it is stored. An MCP server's elicitation
reaches the person through the approval prompter or the approval inbox, its sampling
request runs the harness model with no tools under the run's budget, and its resources
enter the context only through a `Read` tool or an explicit `@server:resource` mention.

| Feature | Default | Adds |
|---|---|---|
| `web` | off in the library, on in `adk-rust` | `web_fetch`, `web_search` (with a `SearchProvider`), screening, `ProviderWebTools` |
| `mcp` | off in the library, on in `adk-rust` | the elicitation, sampling and resource bridges; `mcp_list_resources`, `mcp_read_resource` |
| `web-fixture` | off | a loopback HTTP server stands in for the web in tests; never on in `adk-rust` |

The umbrella crate forwards them as `harness-web` and `harness-mcp`.

## Web

### Tools

| Tool | Declaration | Class | Behaviour |
|---|---|---|---|
| `web_fetch` | `{ url, max_bytes?, as?: "text" \| "markdown" }` | `Web { allowlisted }` from the URL's host | One `GET`; the body becomes text; the result carries the url, final url, status, content type, byte count and SHA-256 |
| `web_search` | `{ query, limit?: ≤ 10, site?: host }` | `Web { allowlisted }` from `site`, other when absent | Runs the operator's `SearchProvider`; returns `[{ title, url, snippet }]` in the same block; fetches no page |

`web_search` is declared only when a `SearchProvider` is set.

### Mode table rows

| Class | ReadOnly | Plan | Ask | Auto |
|---|---|---|---|---|
| Web, allowlisted host | Allow | Allow | Allow | Allow |
| Web, other host | Deny | Deny | RequireApproval | RequireApproval |

A standing rule may cover a web call. The prompt for a host outside the allowlist shows the
host, path and query separately, names the class, and offers one rule: `DomainIn` on the
exact host the call named, never a wildcard the person did not type.

### Configuration

```rust
use std::sync::Arc;
use adk_harness::{HarnessAgent, WebConfig};

let harness = HarnessAgent::builder("harness")
    .model(model)
    .workspace(adk_devtools::Workspace::new("."))
    .web(WebConfig {
        allowed_domains: vec!["docs.rs".into(), "*.rust-lang.org".into()],
        ..WebConfig::default()
    })
    .build()?;
```

| `WebConfig` field | Default | Meaning |
|---|---|---|
| `allowed_domains` | empty | Exact hosts or `*.example.com`; the `DomainIn` grammar |
| `max_bytes` | 1 MiB | Read on the wire; 64 KiB of text are delivered, the rest cut with a marker |
| `max_url_len` | 2,048 | Longest URL sent |
| `max_query_len` | 512 | Longest search query |
| `timeout` | 20 s | Per request |
| `max_redirects` | 5 | Followed within one call |
| `max_fetches_per_turn` | 20 | Further fetches of the turn are refused with `harness.web.turn_cap` |
| `user_agent` | `adk-harness/<version>` | The `User-Agent` header |

### Rules

| Rule | Mechanism |
|---|---|
| `GET` only, no body, no credentials | The harness's own `reqwest` client: no cookie store, no proxy from the environment, no credential from the broker; a new client per request |
| Resolution | The URL parses under the `DomainIn` grammar (no user information, backslashes, percent-encoded hosts or characters outside `[a-z0-9.-]`); the host resolves before connecting, and a loopback, link-local, private, IPv4-mapped or `.local`/`.internal` address is refused as `harness.web.private_target`; a literal of those kinds is hard-denied by the policy before the tool runs |
| Redirects | Followed by the tool: each target is parsed, resolved and re-classified; a redirect off the host, to a host outside the allowlist, is returned as `redirect_to` for a new call; a redirect to a private target is refused |
| Content | `text/html` becomes text (scripts, styles and hidden elements dropped, links kept as `[text](url)`); `text/*` and `application/json` pass; `application/pdf` is a stub naming the bytes and digest; any other type is refused with its mime type |
| Delivery | `{ url, final_url, status, content_type, sha256, bytes, truncated, screened, content }` with `content` inside `<untrusted-content source="web" url="…" sha256="…">` and the closing tag neutralized inside |
| Screening | Injection patterns (imperatives addressed to the agent, tool-call syntax, role markers) label the line `[screened: injection-pattern]`; secret patterns are replaced by `[redacted: secret-pattern]`; both before the result is stored, and recorded as `harness.web.screened` |
| Exfiltration | A URL over `max_url_len`, whose query or fragment matches a secret pattern or carries more than 256 bytes outside the path, is refused before any request; so is a query over `max_query_len` or matching a secret pattern; the record carries a digest, never the value |

### Provider-native search

`ProviderWebTools` is all off. An enabled tool is attached for the harness model's provider
only, declared to a run that starts in Ask or Auto, carries `allowed_domains` where the
provider supports one (Anthropic and OpenAI `web_search`; Gemini `google_search` and
`url_context` have none, so every domain), and is recorded as `harness.web.provider_enabled`
once per run. The request names which path is governed. A tightening to ReadOnly or Plan
while one is live ends the turn with `harness.web.provider_tightened`. These tools run
inside the model: no policy, ledger, screening or per-call record covers them.

```rust
use adk_harness::ProviderWebTools;

let builder = builder.provider_web_tools(ProviderWebTools {
    anthropic_web_search: true,
    ..ProviderWebTools::default()
});
```

Every other provider-executed builtin passed to `.tool(..)` is refused at build: search and
URL context are enabled here, computer use through its pack, and the rest have no governed
form.

## MCP interaction

Servers are declared to the builder as `McpServerSet`s. Operator-declared servers start
with the first run; workspace-declared servers (`.mcp.json` in the workspace, read by
`adk-rust`) start only once the workspace is trusted, and their elicitations are high-risk
approvals.

```rust
use std::collections::HashMap;
use adk_harness::{HarnessAgent, McpInteractionConfig, McpServerSet};
use adk_tool::mcp::McpServerConfig;

let servers = HashMap::from([(
    "docs".to_string(),
    McpServerConfig { command: "docs-mcp".into(), ..McpServerConfig::default() },
)]);
let harness = HarnessAgent::builder("harness")
    .model(model)
    .workspace(adk_devtools::Workspace::new("."))
    .mcp_servers(McpServerSet::operator(servers))
    .mcp_interaction(McpInteractionConfig { sampling: true, ..McpInteractionConfig::default() })
    .build()?;
```

| `McpInteractionConfig` field | Default | Meaning |
|---|---|---|
| `elicitation` | `true` | Prompt or park; `false` declines |
| `sampling` | `false` | Whether servers may sample the harness model |
| `sampling_max_tokens` | 4,096 | Cap on `max_tokens` |
| `sampling_model` | the member model, else the harness model | The model sampling runs on |
| `idle_sampling` | `false` | A request outside any run is refused |
| `resources` | `true` | `mcp_list_resources` and `mcp_read_resource` are declared |
| `max_elicitations_per_call` | 3 | Per tool call |
| `max_sampling_per_call` | 5 | Per tool call |

| Interaction | Behaviour |
|---|---|
| Elicitation, form | A prompt through the `ApprovalPrompter`: "MCP server *name* asks", the message, the schema rendered as typed fields with defaults; the answer is validated against the schema before it is returned; `Deny` declines, `DenyAllRemaining` cancels; never remembered as a rule |
| Elicitation, URL | The prompt shows the URL's host and path, screened as a composed URL; the harness never opens it; the person opens it and approves |
| Elicitation without a prompter | An `ApprovalRequest` named `mcp:<server>/elicitation` in the approval inbox with `allowed_actions = [Edit, Reject]` (`Edit` carries the form answer), `risk` from the server's trust and the run id; the tool call waits for the decision up to `confirmation_timeout`, then declines |
| Elicitation without either | Declined |
| Sampling | Runs on the harness model with no tools, `max_tokens` capped, the server's system prompt delimited as untrusted after one harness line, metered under the run's `RunBudget` and `SpendKey::org(app).with_agent("mcp:<server>")`; a request outside any run is refused unless `idle_sampling` |
| Resources | `mcp_list_resources { server }` and `mcp_read_resource { server, uri }`, class `Read`; contents inside `<untrusted-content source="mcp:<server>" uri="…" sha256="…">` |

Elicitations share the prompter's serial mutex and per-turn cap, so a server cannot flood
prompts. Nothing a server sends changes its trust, its tools' class or any rule.

## adk-rust

| Surface | Behaviour |
|---|---|
| `--web` / `--no-web` | `web_fetch` is declared by default; `--no-web` removes it |
| `--allow-domain HOST` | Repeatable; `web.allow_domains` in the user config too; a project file may only turn web off |
| `/web` | The allowlist; `/web allow HOST` grants a `DomainIn` rule for `web_fetch` to the user |
| `.mcp.json` | Servers in the workspace, started once the workspace is trusted |
| `/mcp` | The servers and their status; `/mcp resources SERVER` lists what one publishes |
| `@server:uri` | In a turn, appends the resource inside its untrusted block, recorded as `harness.mcp.resource.read` with `via: mention` |

## Audit records

| Action | Fields |
|---|---|
| `harness.web.fetched` | functionCallId, phase (request, response, refused), url, host, finalHost, allowlisted, status, bytes, contentSha256, redirects, truncated, screened, refusal (privateTarget, urlTooLong, secretInQuery, …), urlDigest |
| `harness.web.searched` | provider, queryDigest, site, results, hosts, refusal |
| `harness.web.screened` | url, contentSha256, patterns, lines, redactions, action |
| `harness.web.provider_enabled` | provider, tool, allowlist, mode |
| `harness.mcp.elicited` | server, kind, schemaDigest, decision, by, surface (prompt, inbox, none), approvalRequestId |
| `harness.mcp.sampled` | server, spendKey, modelCalls, tokens, costMicroUsd, budgetRemaining, maxTokens, refused |
| `harness.mcp.resource.read` | server, uri, mimeType, bytes, sha256, via (tool, mention) |

## Invariants

| Invariant | Test |
|---|---|
| I46 One plain `GET`, the target resolved and classified before connecting and at every redirect | `adk-harness/tests/web_tests.rs` (private literal, redirect to a private target), `web/domain.rs` unit tests |
| I47 Fetched text reaches the model only delimited, size-capped, labelled and redacted | `web_tests.rs` (injection page), `web_property_tests.rs` (delivered text) |
| I48 A composed URL is capped and refused on a secret pattern, audited by digest | `web_tests.rs` (secret in query), `web_property_tests.rs` (secrets and oversized queries) |
| I49 Provider tools off by default, attached per provider in Ask or Auto with the allowlist, recorded | `web_tests.rs` (provider web tools) |
| I50 An elicitation is answered only through the prompter or the inbox, validated, never auto-accepted | `adk-harness/tests/mcp_interaction_tests.rs` |
| I51 Sampling runs with no tools under the run's budget and the server's spend key | `mcp_interaction_tests.rs` (sampling) |
| I52 Resources enter only through a `Read` tool or a mention, delimited | `mcp_interaction_tests.rs` (resources) |

`examples/harness_agent` runs the H12 scenarios scripted (`web_fetch_allowlist_and_screening`,
`mcp_elicitation_and_sampling`) and live (`live_web_allowlist`).
