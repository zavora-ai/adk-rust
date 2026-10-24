# The personal preset and take over

The personal preset makes the harness an agent for one person rather than for one code
repository: its workspace is a folder of notes and files under the harness home, it has no
host shell, it remembers what you tell it, and its first run asks for quiet time and a daily
spending limit (spec 3.3 §10 H1, §13 Q7). Take over lets a tool ask you for a password or a
one-time code: you type it on your phone or the web home, and it reaches that one tool call —
never the model, the session, a log or the audit record (spec 3.3 §8.2, §10 H2).

## The personal preset

| Setting | Personal | Coding (spec 3.1 §7.19) |
|---|---|---|
| Workspace | `<home>/workspace`, created owner-only; file tools never leave it | The repository you start in |
| Prompt | `PERSONAL_SYSTEM_PROMPT`: notes, lists and drafts; no build or test assumptions | `CODING_SYSTEM_PROMPT` |
| Mode | Ask; on the command line the command's default (`plan` under `up` and `serve`, `ask` elsewhere) unless `--mode` says otherwise | Ask |
| Members | None | None |
| Shell | None on the host; `bash` only in the OS sandbox (`--sandbox`) or a container | Host, behind approval |
| Long-term memory | On | On in the CLI |
| First run | Asks for quiet time and a daily spending limit, keeps them in `<home>/personal.toml`, and shows both on every start | Nothing |

### On the command line

`--preset personal` is a shared flag, so the REPL, `code`, `serve` and `up` all take it. Under
`adk-rust up` the home is the kernel home (`--home`, `[kernel] home`, else the config
directory's `adk-rust`); everywhere else it is the config directory's `adk-rust`. `--dir`
still chooses another workspace.

```bash
adk-rust up --preset personal --mode auto
```

The first start asks two questions. An empty answer takes the suggestion in brackets; `off`
and `none` turn either off.

```text
First run of the personal preset: two limits your agents keep to. You can change them later in personal.toml.
Quiet time — when your agents start nothing on their own and send no notifications (HH:MM-HH:MM, or off) [22:00-07:00]: 22:00-07:00
Daily spending limit — the most your agents spend on their own in a day, in USD (or none) [5]:
Quiet time: 22:00–07:00. Daily spending limit: $5.00 a day.

  store harness.db: sessions, approvals, runs, processes, agenda, schedules, drivers, the activity ledger
  plans checkpointed in harness.db
  work  /Users/you/Library/Application Support/adk-rust/workspace (personal preset: not a code repository, no host shell)
  quiet time 22:00–07:00 · daily spending limit $5.00 a day
ADK-Rust kernel up on http://127.0.0.1:8080
  mode auto · shell none
```

A message to the kernel then works in that folder, wherever the kernel was started:

```bash
curl -s localhost:8080/api/run -H 'content-type: application/json' -d '{
  "appName": "adk-rust", "userId": "'"$USER"'", "sessionId": "s1",
  "newMessage": {"role": "user", "parts": [{"text": "Start a grocery list for me with tea and oats."}]}}'
```

On `claude-sonnet-5-5` the agent listed the empty workspace, wrote `grocery-list.md` there and
answered "I started your grocery list with tea and oats. It's saved as `grocery-list.md` in
the top level of your workspace."

| Answer | What `adk-rust up` does with it |
|---|---|
| Quiet time | The kernel's quiet hours: no proactive or maintenance process starts inside the window ([Proactivity](../kernel/proactivity.md)) |
| Daily spending limit | Every partition's daily spend cap: background work is admitted only while the day's spend stays within it ([Scheduling](../kernel/scheduling.md)) |

`[kernel] quiet_hours` and `[kernel.quotas] daily_spend_cap_usd` in your user config win over
the first-run answers; the banner shows the values in force. Edit `personal.toml`, or delete it
to be asked again.

> **Note:** a first run without a terminal fails and names the file: write
> `quiet_time = "22:00-07:00"` and `daily_spending_limit_usd = 5.0` into `<home>/personal.toml`.
> `adk-rust up --install --preset personal` asks its questions before it installs the service,
> and the service runs in the personal workspace.

### In code

```rust
use std::sync::Arc;

use adk_rust::harness::HarnessAgent;
use adk_rust::harness::personal::PersonalAgent;
use adk_rust::model::GeminiModel;

fn build() -> anyhow::Result<Arc<HarnessAgent>> {
    let model = Arc::new(GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?);
    let agent = PersonalAgent::builder()
        .name("assistant")
        .model(model)
        .home("/var/lib/my-assistant")
        .build()?;
    // Notes and files land in /var/lib/my-assistant/workspace.
    Ok(agent.into_harness())
}
```

`PersonalAgentBuilder::configure` reaches every other `HarnessAgentBuilder` setting — stores,
an approval service, take over. Any builder takes the preset with
`HarnessAgentBuilder::preset(Preset::Personal)`; `Preset::Coding` is the coding agent's lowering.
`PersonalSettings::load_or_ask(home, input, output)` runs the first-run questions over any
reader and writer, for a program that owns its own first run.

## Take over

A tool that needs a secret asks for it from inside its call. The request reaches every surface
as a decision of its own kind; the person types the secret, and the tool gets it.

| Step | What happens | What is kept |
|---|---|---|
| Ask | The tool calls `TakeOver::ask(ctx, TakeOverAsk::one_time_code(heading, resource))` | An inbox entry with tool name `harness:take-over`: the heading, the tool, the resource, the kind of secret. `harness.take_over.requested` in the audit log |
| Take over, then Done | A surface calls `TakeOver::provide(id, version, TakeOverAnswer::new(person, TakeOverSurface::Phone, secret))` | The entry approved by the person, reason "taken over on the phone". `harness.take_over.provided` naming who and where |
| Use | The tool receives a `ScopedToken` bound to the resource; `authorize_for` refuses any other | Nothing: the value lives in memory for that call |
| Answer | The call's result and error reach the model with the secret replaced by `[secret typed by the person]` | The scrubbed result, as any other |

```rust
use std::sync::Arc;

use adk_rust::harness::personal::PersonalAgent;
use adk_rust::harness::take_over::{TakeOver, TakeOverAsk};
use adk_rust::harness::HarnessAgent;
use adk_rust::serde_json::{Value, json};
use adk_rust::{ApprovalService, Llm, Result, Tool, ToolContext, async_trait, resource_indicator};

/// Signs in to a bank with the one-time code the person types.
struct BankSignIn {
    take_over: TakeOver,
}

#[async_trait]
impl Tool for BankSignIn {
    fn name(&self) -> &str {
        "bank_sign_in"
    }

    fn description(&self) -> &str {
        "Signs the person in to their bank; they type the one-time code themselves."
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let bank = resource_indicator("https://bank.example.com/login")?;
        let ask = TakeOverAsk::one_time_code("Type the one-time code your bank sent", bank.clone());
        let token = self.take_over.ask(ctx.as_ref(), ask).await?;
        let code = token.authorize_for(&bank)?;
        // Hand `code.expose()` to the bank's client here; the result goes to the model
        // scrubbed of it.
        Ok(json!({ "signedIn": true, "codeLength": code.expose().len() }))
    }
}

/// A personal agent whose tools may take over, through `inbox`.
fn build(model: Arc<dyn Llm>, inbox: Arc<dyn ApprovalService>) -> anyhow::Result<Arc<HarnessAgent>> {
    let take_over = TakeOver::new();
    let tool = Arc::new(BankSignIn { take_over: take_over.clone() });
    let agent = PersonalAgent::builder()
        .model(model)
        .home("/var/lib/my-assistant")
        .tool(tool)
        .configure(move |builder| builder.approval_service(inbox).take_over(take_over))
        .build()?;
    Ok(agent.into_harness())
}
```

`take_over` binds the take over to the harness's inbox and confirmation timeout and scopes
every tool call, so a secret can reach the call that asked and is scrubbed from what it
returns.

A surface finds the entry with `TakeOverRequest::from_request` over the inbox, offers **Take
over** while `TakeOver::is_waiting(id)`, and answers through `HarnessRuntime::take_over()`.
`TakeOver` is also a `CredentialBroker`: a tool written against `token_for` gets a token the
person types, for the agent running the call only.

### What it guarantees

| Guarantee | How |
|---|---|
| The model never sees the secret | Every occurrence in the call's result and error — as typed and percent-encoded, in values and keys — becomes `[secret typed by the person]` before the action ledger, hooks, members or the model see it |
| No store keeps it | The inbox entry carries the heading and resource only; the decision records who and where; sessions, the action ledger and the audit log get the scrubbed result |
| No log line carries it | The harness logs request ids, tools and surfaces; `CredentialSecret`'s `Debug` and `Display` are redacted |
| Only the session's person answers | The entry's approver is the session's user; anyone else gets `approval.not_an_approver` and the call keeps waiting |
| One answer | A second answer gets `approval.version_conflict` or `approval.already_resolved`, never a retry |
| Phone and web only | `TakeOverSurface` has `Web` and `Phone`; chat, voice and the tray cannot carry a secret (spec 3.3 §8.2) |
| Nothing waits forever in flight | The call waits up to the harness's confirmation timeout; then, on **Deny**, on **Stop all** or when the call is cancelled, the entry is cancelled and the tool gets `harness.take_over.declined` (or the freeze) |

> **Important:** a take over is a live exchange. The secret is held in memory by the process
> running the call, so a restart ends it and `provide` answers `harness.take_over.not_waiting`;
> the agent asks again. A one-time code does not outlive a night of quiet time, so a take over
> never waits through one.

The tool itself must not write the secret to session state, progress events or its own logs;
the harness scrubs what the call returns.

| Error code | When |
|---|---|
| `harness.take_over.unavailable` | Asked outside a call of a harness built with `take_over`, or the harness has no approval service |
| `harness.take_over.declined` | The person denied it, the entry expired or was cancelled, the timeout passed, or the call was cancelled |
| `harness.take_over.not_waiting` | No call in this process waits for the entry |
| `harness.take_over.invalid_secret` | Fewer than 4 or more than 4,096 characters, or control characters |

### Tests that prove it

| Test | Shows |
|---|---|
| `adk-harness/tests/take_over_tests.rs` `a_one_time_code_reaches_the_tool_and_nothing_else` | On one SQLite file: the bank gets the code once; the model sees the placeholder; no byte of `harness.db` — sessions, inbox, signed audit log — holds the code |
| `the_secret_never_appears_in_any_persisted_event_or_log_line` | Property, 100 cases: random secrets reach the tool and appear in no event, store record, audit record, model request or log line at any level |
| `only_the_session_owner_takes_over_and_a_second_answer_conflicts`, `a_declined_take_over_answers_the_call_without_a_secret`, `a_take_over_nobody_answers_times_out_and_leaves_no_entry_open`, `stop_all_ends_the_wait` | Who may answer, and every way a take over ends without a secret |
| `live_take_over_with_claude` (ignored) | On `claude-sonnet-5-5`: the model calls the tool, the phone types the code, and the model reports the receipt with the placeholder |
| `adk-cli/tests/harness_personal_cli.rs` | `adk-rust up --preset personal` started in a repository asks once, works in the personal workspace with no shell, and shows both limits on every start |

## Related

- [The harness in your terminal](cli.md)
- [Modes and approvals](modes-and-approvals.md)
- [The kernel](../kernel/index.md)
- Spec: `docs/design/spec-3.3-harness-ui.md` §8.2, §10 H1–H2, §13 Q7
