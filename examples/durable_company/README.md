# Durable Company

One accounts-payable day of a small company, run end to end on the durable runtime against a
real model. An agent pays the vendor invoices due today; the infrastructure around it keeps the
day correct through a crash, an approval, a spend cap, and a freeze, and every process leaves a
signed audit log.

The example asserts on what the stores hold — run state, attempts, checkpoints, approval
requests, the spend ledger, the bank's payment ledger, and the exported audit logs — never on
model prose, and prints each step's evidence.

## The day

| Step | What happens | ADK pieces |
|------|--------------|------------|
| 1. Cron fires | The `ap-daily` schedule (`0 9 * * *` UTC, first occurrence due now) fires on replica A's scheduler and enqueues one run | `DurableScheduler`, `ScheduleStore` |
| 2. Crash | Replica A's worker claims the run, the clerk calls `list_due_invoices`, and the process is killed with SIGKILL inside the call | `DurableWorker`, fenced lease, `RunCheckpointRef::Session` |
| 3. Resume | Replica B claims the run once A's lease expires (attempt 2), answers the interrupted call `outcome_unknown`, and continues the same session | `Runner::recover_unanswered_calls`, `DurableRunStore` |
| 4. Policy | `INV-1041` (300 USD) is paid; `INV-1042` (1,200 USD) needs approval, so the run parks `Waiting` | `DeclarativePolicy`, `AgentPrincipal`, `RunCheckpointRef::AwaitingApproval` |
| 5. Approval | The requester's own approval is refused; one finance approval keeps the run parked; the second meets the quorum and wakes it | `StoreApprovalService` (quorum 2) |
| 6. Payment | The resumed run pays `INV-1042` once through the governed checkout | `adk-payments` checkout tools, `ActionLedger` |
| 7. Spend cap | A rush run asks for `INV-1043` (800 USD): the policy allows it, the 2,000 USD daily cap refuses it before the bank | `SpendLedger` with `SpendLimits` |
| 8. Freeze | The CFO freezes the org: a new run stays queued and a due schedule does not fire on any replica | `GovernanceControl::mirrored`, `freeze_recorded` |
| 9. Audit | Each process's log (`audit/worker-a`, `audit/worker-b`, `audit/console`) is exported and verified; a copy with one decision edited is rejected | `AuditLog`, `adk-rust audit export`, `adk-rust audit verify` |

## Company setup

| Setting | Value |
|---------|-------|
| Agent identity | `spiffe://acme.example/agent/ap-clerk`, tenant `acme`, instance = worker id |
| Policy `acme-ap-v1` | `list_due_invoices` allowed; `pay_invoice` allowed up to 1,000 USD; above that, approval required; anything else denied — every rule only for this agent and tenant |
| Approvals | Group `finance`, quorum 2, self-approval forbidden, expiry 1 hour |
| Spend cap | 2,000 USD per day for the org, covering model calls and payments |
| Lease | 4 seconds; a killed worker's run is recovered within that |
| Bank | `payments.jsonl` in the run directory; every process appends completed payments to it |

## Run

```bash
cp examples/durable_company/.env.example examples/durable_company/.env
# set OPENAI_API_KEY and/or ANTHROPIC_API_KEY

# SQLite (default): replicas are separate OS processes sharing one database file
cargo run --manifest-path examples/durable_company/Cargo.toml -- --provider openai

# PostgreSQL: each run creates its tables in a new schema, durable_company_<tag>
DATABASE_URL=postgres://postgres@localhost:5432/postgres \
  cargo run --manifest-path examples/durable_company/Cargo.toml -- --provider anthropic --store postgres

# In memory: replicas are tasks in one process, and the "kill" aborts one
cargo run --manifest-path examples/durable_company/Cargo.toml -- --provider openai --store memory
```

| Flag | Default | Meaning |
|------|---------|---------|
| `--provider` | `openai` | `openai` (`gpt-5.6-luna`, Responses API) or `anthropic` (`claude-haiku-5-5`); `OPENAI_MODEL` and `ANTHROPIC_MODEL` override the model |
| `--store` | `sqlite` | `memory`, `sqlite`, or `postgres` (`DATABASE_URL` or `--database-url`) |
| `--data-dir` | `target/durable-company/<tag>` | The run directory: database file, audit key, exports, payments, worker logs |

To export and verify with the CLI rather than the library, build it and point the example at it:

```bash
cargo build -p adk-cli --features audit-sql
ADK_RUST_BIN=target/debug/adk-rust cargo run --manifest-path examples/durable_company/Cargo.toml
```

The run directory keeps everything needed to check the result by hand:

```bash
cd target/durable-company/<tag>
adk-rust audit export --database-url sqlite://$PWD/company.db --log-id audit/worker-b --out b.jsonl
adk-rust audit verify b.jsonl --key "$(cat audit.vkey)"
cat payments.jsonl      # one line per payment, with the paying process id
cat worker-b.log        # the replica's tracing output
```

## Expected output

Abridged from a SQLite run with `--provider openai`:

```text
[   0.4s] cron fired: run 651fbfd8-… enqueued with key schedule:ap-daily:2026-10-12T11:39:12.099048Z; schedule advanced to 2026-10-13T09:00:00+00:00
[   2.4s] crash: worker-a (pid 88764) killed with SIGKILL inside list_due_invoices; run 651fbfd8-… still Running at attempt 1 owned by worker-a, checkpoint Session
[   6.1s] resume: worker-b claimed run 651fbfd8-… 3.7s after the kill (lease 4s), attempt 2
[   9.9s] park: run 651fbfd8-… Waiting (attempt 2), 1 held call(s); INV-1041 (300 USD, under the threshold) paid 1x; INV-1042 (1200 USD) paid 0x
[  11.9s] approval 1/2: … controller@acme.example refused with approval.self_approval; dana@acme.example approved -> Pending, run still Waiting, 0 payments
[  13.7s] approval 2/2: eli@acme.example approved -> Approved; run woke and completed 1.9s later at attempt 2; payments: INV-1041 1x, INV-1042 1x
[  16.8s] spend cap: … INV-1043 (800 USD) allowed by policy, refused by the 2000 USD/day cap before the bank, 0 payments
[  24.3s] freeze: cfo@acme.example froze the org (epoch 1, frozen true); 6s later run 9600d80d-… is still Queued at attempt 0, 'month-end-close' … has not fired
[  24.6s] audit: audit/worker-b: 18 records […, list_due_invoices outcome_unknown x1, …]; exported by adk-rust audit export; adk-rust audit verify (exit 0): signed through: 18; unsigned tail: 0; result: VALID
[  24.6s] audit: audit/worker-b with one decision edited: adk-rust audit verify (exit 1): … result: INVALID (19 failures)

PASS: 16 steps, evidence in target/durable-company/f47dc18e
```

> **Note:** the scenario fails loudly when a model does not follow its directive prompt — for
> example, if it never calls `list_due_invoices` — and prints the worker's log tail. Rerun it;
> nothing is shared between runs.
