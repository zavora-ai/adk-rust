# Harness identity and credentials

The harness knows who it acts for: roles decide which tool classes a user may run,
`git` and HTTPS tools get short-lived tokens for that user, a served harness gives standing
rules and Auto only to users it authenticated, and every run writes a tamper-evident audit
trail. Feature `auth` of `adk-harness`, on by default (spec 3.1 §7.14, §9.2 I6, I39).

```toml
[dependencies]
adk-harness = { version = "3.0.0", features = ["auth"] }
```

## Who a run acts for

Every run has an `ActingContext` — the acting agent, the subject it acts for, the
delegation chain — resolved by the runner from `RunConfig::acting`. The harness carries it
into every record it writes:

| Record | Carries it as |
|---|---|
| `ToolPolicyRequest`, `ToolConfirmationRequest` | `acting` |
| Spend | `SpendKey::agent`, and the subject on each `SpendEntry` |
| `harness.*` audit records | `actor.agent`, `actor.on_behalf_of`, `actor.delegation_chain`, `actor.invocation_id` |

Tools and callbacks inside an `LlmAgent`, workflow agents, team members, agent tools,
graph nodes and realtime agent tools all see `ReadonlyContext::is_authenticated()` of the run
they belong to.

## Roles: `RbacMap`

`HarnessAgentBuilder::rbac(map)` wraps every harness tool whose class the map names in
`adk-auth`'s `ScopeGuard`, with the permission as the required scope; scopes come from the
run's authenticated request (`ToolContext::user_scopes`). `RbacMap::with_access_control`
also wraps the tools in `ProtectedTool`, which checks the user's roles for the tool name.

| Class (`RbacMap::harness_defaults`) | Permission |
|---|---|
| `Shell(Host)`, `Shell(OsSandbox)`, `Shell(Container)`, `ShellEscalation` | `harness:shell` |
| `Edit` | `harness:edit` |
| `MemoryWrite` | `harness:memory` |
| `Payment` | `harness:pay` |

The check runs when the tool executes, after the policy and any approval, so a denial is
the tool's error. Its code names the permission — the part of a tool error the model sees:
`tool 'bash' failed: tool.forbidden (harness.rbac.denied:harness:shell)`. The wrappers
forward the tool's declaration and annotations, so the declaration digest, standing rules
and held calls are unchanged.

```rust
use std::sync::Arc;

use adk_harness::{HarnessAgent, HarnessMode, Isolation, RbacMap, ToolKind};

fn served(model: Arc<dyn adk_core::Llm>) -> adk_core::Result<HarnessAgent> {
    HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
        .mode(HarnessMode::Ask)
        .rbac(RbacMap::harness_defaults().require(ToolKind::Shell(Isolation::Host), "ops:shell"))
        .require_authenticated_identity()
        .build()
}
```

## Credentials for the shell

`HarnessAgentBuilder::credentials(broker)` mints a token per `bash` call through an
`adk_core::CredentialBroker` — `adk-auth`'s `TokenExchangeBroker` with `on_behalf_of` for
per-user tokens — for the subject the run acts for, for each `ShellCredential` added with
`shell_credential`, or else for each HTTPS remote in the workspace's `.git/config`.

| Step | What happens |
|---|---|
| Before the call | tokens are minted; a helper script that prints them is written to a private temporary directory |
| The call | `bash` gets `GIT_ASKPASS` and a git credential helper (`GIT_CONFIG_*`) pointing at the script, and `ADK_CREDENTIAL_HELPER`; the token itself is never in the shell's or the harness's environment |
| After the call | the script is deleted and every token is revoked (`RevocationScope::AgentResource`) |

A resource the broker cannot mint for is skipped with a warning; the command runs without it.

```rust
use std::sync::Arc;

use adk_harness::{HarnessAgent, ShellCredential};

fn with_github(
    model: Arc<dyn adk_core::Llm>,
    broker: Arc<dyn adk_core::CredentialBroker>,
) -> adk_core::Result<HarnessAgent> {
    HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
        .credentials(broker)
        .shell_credential(ShellCredential::new("https://github.com/")?.with_scopes(["repo"]))
        .build()
}
```

## Authenticated identity on a server

`require_authenticated_identity()` marks the harness as served. A run whose user the
server's `RequestContextExtractor` did not authenticate (`ReadonlyContext::is_authenticated`
is `false`) is decided as follows:

| Mode | Decided as |
|---|---|
| Auto | Ask |
| ReadOnly, Plan, Ask | unchanged |
| Standing rules | none apply |

Without the switch — the CLI, where the operating-system user is the user — rules and
modes apply as configured.

## Audit trail

Every harness run records to an audit trail in `AuditFailureMode::Block`: an action whose
record cannot be written does not happen.

| Source of the trail | Used when |
|---|---|
| The runner's `audit_log` | the runner has one |
| `HarnessAgentBuilder::audit_log` | the run has none; also installed on runners built with `HarnessAgent::configure_runner` |
| `EphemeralAuditLog` | nothing is configured: a signed, Merkle-hashed `adk_auth::signed_audit::AuditLog` in memory with a key generated at start-up |

A run whose trail is in `Warn` mode is refused with `harness.audit_required` (I39). For a
deployment, pass an `AuditLog` over a durable `AuditLogStore` (SQLite or PostgreSQL in
`adk-session`); `EphemeralAuditLog` loses its records when the process exits.

```rust
use std::sync::Arc;

use adk_auth::signed_audit::{AuditLog, Ed25519Signer};
use adk_core::audit_log::InMemoryAuditLogStore;
use adk_harness::HarnessAgent;

async fn audited(model: Arc<dyn adk_core::Llm>) -> adk_core::Result<HarnessAgent> {
    let signer = Arc::new(Ed25519Signer::generate("audit.example.com").map_err(|e| adk_core::AdkError::config(e.to_string()))?);
    let log = AuditLog::builder(Arc::new(InMemoryAuditLogStore::new()), "replica-1", signer).open().await?;
    HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new("."))
        .audit_log(Arc::new(log))
        .build()
}
```
