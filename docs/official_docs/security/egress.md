# Network Egress Policy

`adk_core::egress` decides which hosts, ports and addresses a connection may reach. One
`EgressPolicy` describes the destinations; framework HTTP clients enforce it through
`GuardedClient`; one address classifier, `classify`, is shared by every check in the
workspace.

| Part | Type | Feature |
|------|------|---------|
| Policy and decision | `EgressPolicy`, `EgressRule`, `EgressTarget`, `EgressDenial` | — |
| Address classifier | `classify`, `is_non_public`, `AddressClass` | — |
| Name resolution | `HostResolver`, `StaticResolver`, `SystemResolver` | `SystemResolver`: `egress-dns` |
| Guarded HTTP client | `GuardedClient`, `GuardedClientBuilder`, `EgressResolver` | `egress-client` |
| Egress proxy for sandboxed processes | `adk_sandbox::egress_proxy::EgressProxy`, `EgressRecord`, `EgressAudit` | `adk-sandbox`: `egress-proxy` |

```toml
[dependencies]
adk-core = { version = "3.0.0", features = ["egress-client"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Rules

A policy is a list of rules plus two switches. A rule without a port admits every port.

| Rule | Text | Admits |
|------|------|--------|
| Any host | `*`, `*:443` | Every host name, and IP literals whose address passes the address check |
| Exact host | `api.example.com`, `api.example.com:443` | That name |
| Subdomains | `*.example.com`, `*.example.com:443` | Every name below `example.com`, not `example.com` itself |
| CIDR | `10.0.0.0/8`, `10.0.0.5:5432`, `fd00::/8`, `[fd00::/8]:443` | IP literals in the range on the port, and names that resolve into the range |

| Constructor | Admits |
|-------------|--------|
| `EgressPolicy::deny_all()` | Nothing until rules are added |
| `EgressPolicy::public_only()` (the default) | Any host over `https`, on public addresses |
| `EgressPolicy::allowlist(rules)` | Exactly the rules, over `https` |
| `EgressPolicy::allow_private_networks()` | Any host over `http` or `https`, on any address except cloud metadata; for in-cluster receivers and tests |

| Switch | Effect |
|--------|--------|
| `with_private_networks()` | Admits loopback, private and other non-public addresses for admitted hosts |
| `with_plain_http()` | Admits `http` URLs |

## Decisions

`EgressPolicy::decide` is a pure function of the policy and an `EgressTarget`:

| Target | Checked | When |
|--------|---------|------|
| `Name { host, port }` | A host rule admits the name and port | Before resolution |
| `Address { host, address }` | The address passes the address check | For every address the name resolves to, at connect time |
| `Literal(address)` | A CIDR rule admits the address and port, or an any-host rule admits the port and the address passes the address check | Before connecting |

| Address class | Address check |
|---------------|---------------|
| `Metadata` | Always refused, whatever the rules say |
| `Public` | Admitted |
| Every other class | Admitted only inside a CIDR rule, or with `with_private_networks()` |

`EgressPolicy::check_url` combines the scheme check with the `Name` or `Literal` check of a
URL. `localhost` and `*.localhost` names are checked as loopback before resolution. A host name
whose last label is numeric (`127.1`, `0x7f.1`, `2130706433`) is refused, since `inet_aton`-style
resolvers read it as an IPv4 address.

Every refusal is an `EgressDenial` whose `Display` form is the guidance a caller reads and whose
`code()` is a stable identifier such as `egress.host_not_allowed`. `find_denial` recovers it
from an error's source chain.

```rust
use adk_core::egress::{EgressDenial, EgressPolicy, EgressTarget};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let policy = EgressPolicy::allowlist(["api.github.com:443", "*.pythonhosted.org:443"])?;

    assert!(policy.check_url("https://files.pythonhosted.org/simple/").is_ok());
    assert!(matches!(
        policy.check_url("https://example.com/"),
        Err(EgressDenial::HostNotAllowed { .. })
    ));
    assert!(matches!(
        policy.decide(&EgressTarget::Address {
            host: "api.github.com",
            address: "10.0.0.7".parse()?,
        }),
        Err(EgressDenial::AddressNotAllowed { .. })
    ));
    Ok(())
}
```

## Narrowing

`narrow` intersects two policies: a destination passes the result only when it passes both,
and both switches stay on only when both policies set them. An operator's policy is the
ceiling; every lower layer narrows it and none can widen it. A narrowed policy can only be
narrowed further: `allow` and the switches leave it unchanged.

```rust
use adk_core::egress::EgressPolicy;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let operator = EgressPolicy::allowlist(["*.example.com:443"])?;
    let tool = EgressPolicy::allowlist(["api.example.com", "evil.test"])?;
    let effective = operator.narrow(&tool);

    assert!(effective.check_url("https://api.example.com/v1").is_ok());
    assert!(effective.check_url("https://evil.test/").is_err());
    assert!(effective.check_url("https://docs.example.com/").is_err());
    Ok(())
}
```

## Address classes

`classify` is the one classifier every egress check uses — webhook delivery, AWP event
delivery, the A2A push sender, Web Bot Auth directories, website migration, graph HTTP action
nodes, the harness `web_fetch` tool and browser navigation. An IPv6 address embedding an IPv4
address — IPv4-mapped, NAT64 or 6to4 — has the class of the address it embeds.

| Class | IPv4 | IPv6 |
|-------|------|------|
| `Metadata` | `169.254.169.254`, `169.254.170.2`, `169.254.170.23`, `100.100.100.200`, `168.63.129.16` | `fd00:ec2::254`, `fd00:ec2::23` |
| `Unspecified` | `0.0.0.0/8` | `::` |
| `Loopback` | `127.0.0.0/8` | `::1` |
| `Private` | `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` | `fc00::/7`, `fec0::/10` |
| `LinkLocal` | `169.254.0.0/16` | `fe80::/10` |
| `SharedAddressSpace` | `100.64.0.0/10` | — |
| `Multicast` | `224.0.0.0/4` | `ff00::/8` |
| `Reserved` | `240.0.0.0/4`, `198.18.0.0/15`, `192.0.0.0/24` | `100::/64`, IPv4-compatible and IPv4-translated forms |
| `Tunnel` | — | Teredo `2001::/32`, local-use NAT64 `64:ff9b:1::/48` |
| `Public` | everything else | everything else |

## Guarded HTTP client

`GuardedClient` (feature `egress-client`) enforces a policy on every request it sends:

| Check | Where |
|-------|-------|
| Scheme, host name and port, IP literals | Before the request is built (`check_url`) |
| Every address a name resolves to | At connect time, through `EgressResolver`: the address checked is the address connected to, and one refused address refuses the whole answer |
| Every redirect hop | Scheme, host, port and literal again, then resolution again |
| Proxies | None: environment and system proxy settings are ignored, since a proxy resolves names itself |
| Build failure | `EgressClientError`; no unguarded client is used in its place |

```rust,no_run
use std::time::Duration;

use adk_core::egress::{EgressPolicy, GuardedClient, find_denial};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = GuardedClient::builder(EgressPolicy::allowlist(["api.github.com:443"])?)
        .timeout(Duration::from_secs(10))
        .redirects(3)
        .build()?;

    let request = client.get("https://api.github.com/zen")?;
    match request.send().await {
        Ok(response) => println!("{}", response.status()),
        Err(error) => match find_denial(&error) {
            Some(denial) => println!("refused ({}): {denial}", denial.code()),
            None => println!("failed: {error}"),
        },
    }
    Ok(())
}
```

`GuardedClientBuilder::resolver` takes any `HostResolver`; `StaticResolver` maps fixed names to
addresses for tests. A crate that needs its own `reqwest` client options sets
`EgressResolver` with `ClientBuilder::dns_resolver` and `ClientBuilder::no_proxy`, and checks
URLs and redirect hops with `check_parsed_url` itself.

## Egress proxy for sandboxed processes

`adk_sandbox::egress_proxy::EgressProxy` (feature `egress-proxy` of `adk-sandbox`) enforces a
policy on a child process's connections. The sandbox lets the process reach only the proxy's
loopback port (network access `Proxied`), and the standard proxy variables point it there.

| Request | Handling |
|---------|----------|
| `CONNECT host:port` | The name and port are checked; the proxy resolves the name itself, checks every address, and connects only to an address it checked. The TLS `ClientHello` must name `host` in its SNI: no SNI, Encrypted Client Hello, or a stream that is not TLS is refused. A `CONNECT` to an IP literal is admitted by the policy's address rules alone |
| `GET http://host/path` (absolute form) | Only when the policy admits plain `http`; checked the same way, forwarded with `Host` set to the checked authority and hop-by-hop headers removed |
| Loopback or unspecified destinations | Always refused, so the proxy never connects to itself or to the host's local services |
| Anything else | `400 Bad Request` |

A refusal answers `403 Forbidden` with the reason and an `X-Adk-Egress-Denied` header carrying
the denial code (`egress.host_not_allowed`, `egress.sni_mismatch`, …). Every connection is one
`EgressRecord` — call id, host, port, connected address, SNI, outcome and byte counts — for the
proxy's `EgressAudit`, and one `tracing` event. Start one proxy per sandboxed call so each record
names its call; dropping the proxy closes its listener and every open connection.

```rust,no_run
use std::sync::Arc;

use adk_core::egress::EgressPolicy;
use adk_sandbox::egress_proxy::{EgressProxy, InMemoryEgressAudit};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let audit = Arc::new(InMemoryEgressAudit::new());
    let proxy = EgressProxy::builder(EgressPolicy::allowlist(["pypi.org:443", "*.pythonhosted.org:443"])?)
        .call_id("call-42")
        .audit(audit.clone())
        .bind()
        .await?;
    println!("HTTPS_PROXY=http://127.0.0.1:{}", proxy.port());
    // Run the sandboxed command; then read what it reached.
    for record in audit.records() {
        println!("{:?} {}:{} {:?}", record.protocol, record.host, record.port, record.outcome);
    }
    Ok(())
}
```

## Tool declarations

An operator sets the egress ceiling for a run on the runner or the run's configuration; every
lower layer can only narrow it.

| Where | Effect |
|-------|--------|
| `Runner::builder().egress_policy(policy)` | The ceiling of every run; a run's own policy is intersected with it |
| `RunConfig::builder().egress_policy(policy)` | Sets the run's policy, or narrows the one already set |
| `ToolContext::egress_policy()` | The run's policy, for a tool that opens its own connections, for example through a `GuardedClient` built from it |

A tool declares the destinations of a call with `Tool::egress(&args)`. Under a run egress policy,
`authorize_tool_call` checks the declaration before the tool policy, guardrails, or
confirmation, and no approval admits a call it refuses:

| `ToolEgress` | Under a run egress policy |
|--------------|---------------------------|
| `Undeclared` (the default) | Refused |
| `None` | Admitted: the call opens no network connection |
| `Configured` | Admitted: the call reaches only endpoints the operator fixed when building the tool, such as a database or a model provider (`AgentTool` declares this; the delegate's own calls are checked against the policy the delegation carries) |
| `Enforced` | Admitted: every connection is checked when it is made, by a `GuardedClient` built from the run's policy or by a sandbox's egress proxy |
| `Destinations(rules)` | Admitted when the policy covers every rule (`EgressPolicy::covers`); `ToolEgress::for_urls` builds the rules from URLs |

`FunctionTool::with_egress` and `with_egress_fn` declare a function tool's destinations, the
second from its arguments. A tool wrapper forwards its inner tool's declaration.

```rust
use std::sync::Arc;

use adk_core::egress::{EgressPolicy, ToolEgress};
use adk_core::{RunConfig, Tool};
use adk_tool::FunctionTool;
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fetch = Arc::new(
        FunctionTool::new("fetch_page", "Fetches a page", |_ctx, args| async move { Ok(args) })
            .with_egress_fn(|args| ToolEgress::for_urls([args["url"].as_str().unwrap_or_default()])),
    );
    let config = RunConfig::builder()
        .egress_policy(EgressPolicy::allowlist(["docs.rs:443", "*.github.com:443"])?)
        .build();
    let policy = config.egress_policy.as_ref().expect("set above");

    assert!(policy.admits_tool(&fetch.egress(&json!({ "url": "https://docs.rs/adk-core" }))).is_ok());
    assert!(policy.admits_tool(&fetch.egress(&json!({ "url": "https://evil.test/" }))).is_err());
    Ok(())
}
```

What a declaration does not cover:

- A declaration is the tool author's statement. Only connections made through a
  `GuardedClient` or a sandbox's egress proxy are enforced when they are made.
- The declaration is computed from the arguments the agent built; arguments a tool guardrail
  rewrites or an approver edits are not declared again.
- Provider-executed built-in tools (`GoogleSearchTool`, `OpenAIWebSearchTool`, …), model
  providers, and stores the operator configures are outside the policy.
- Governed graph nodes and native realtime handlers declare nothing, so a run egress policy
  refuses them.

## Limits

- **Not data-loss prevention.** An admitted host receives whatever is sent to it.
- **Not proof against domain fronting.** A host rule matches the name a client asks for; a
  shared CDN can route that connection to another tenant.
- **Only checked connections are covered.** A socket opened by code that does not consult the
  policy — including third-party code running in the agent's process — is not contained. A
  child process is contained only when its sandbox enforces `Proxied` network access, so that
  the proxy is the one destination it can reach.
