- **The kernel's agenda and scheduling** (`adk-kernel`, `adk-harness`; spec 3.2 phase 2):
  the agent proposes its own follow-up work, under control, and a human turn preempts
  background work.
  - `Kernel::agenda` returns the `Agenda`: `propose`, `approve`, `reject`, `schedule`,
    `list`. An `AgendaItem` goes `proposed` → `approved` → `scheduled` → `running` →
    `done`, or `rejected` or `expired`, and runs as a `Proposed` process whose parent is the
    proposing run. `ProactivityPolicy` (`KernelConfig::proactivity`) routes proposals by the
    partition's mode — held in ReadOnly and Plan, to the approvals inbox as a
    `kernel:agenda` request with approve, edit and reject in Ask, scheduled in Auto — and
    rejects one past `max_proposed_open` or the daily proactive budget with a reason on the
    bus. An item from the consolidation tick waits for a person in every mode.
  - `AgendaStore` on in-memory, SQLite, PostgreSQL and Redis, with
    `agenda_store_conformance_tests!`; `ConsolidationTick` holds the tick's settings.
  - Admission at every claim: per-partition `Quotas` (`max_running`, `max_queued`,
    `daily_spend_cap`), `QuietHours` for proactive and maintenance processes (through
    `not_before` for agenda items), cost-aware admission against the day's spend with
    `BudgetReplenished` wakes, and preemption of the youngest background process when an
    interactive one finds its partition at quota; a held process sleeps with a
    `kernel.admission` label saying why.
  - `KernelEvent::Agenda`, `kernel.agenda.*` audit records, and `GET /api/kernel/agenda`
    with `POST …/{approve,reject,schedule}` (operator scope, `kernel.control` records) and
    the matching `ControlClient` calls.
  - A daily review on the maintenance tick (`ProactivityPolicy::review_period`, 90 days):
    standing rules with no expiry older than the period, with a renewal ask, and sleepers and
    freeze-parked processes older than it, as `KernelEvent::Review` and one `kernel.review`
    record; it changes nothing. Rules are read through `StandingRuleSource`, which
    `Harness::kernel` sets from the harness's rule store.
  - With the `kernel` feature, the harness declares `propose_followup { title, rationale,
    when?, steps? }`, class `Todo` in every mode; the follow-up runs in its own session in
    the proposing session's mode.
