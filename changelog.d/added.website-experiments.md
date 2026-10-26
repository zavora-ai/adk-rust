- **Experiments and measurement sources for `adk-website`** (spec 3.5 §7.12, A-I15, S11):
  `experiments` maps the charter's KPIs and guardrails onto `adk-eval`'s
  `ExperimentAnalyzer` (a guardrail's tolerance is the charter's headroom from its KPI
  baseline), builds each variant as its own static file through `ExperimentGenerator`,
  assigns tabs by a stable hash of an anonymous bucket in `/_awp/experiments.js`, counts
  exposures and goals through the edge's beacon, feeds the cumulative counts to the analyzer
  once a day (`ExperimentRunner`, records in `InMemoryExperimentStore` or
  `FileExperimentStore`), pauses the job and keeps the control on a guardrail breach, and puts
  the decision and evidence in the weekly review (`ReviewComposer::with_experiments`).
  `measurement` adds the KPI sources that are not drivers: `BeaconKpi`, `HostLogs` (Common,
  Combined and W3C extended logs, no addresses kept), `CommerceKpi` over
  `MeteredActionLedger`, `SpendLedgerKpi` over `MeteredSpendLedger`, `AgentSearchKpi` over
  `EdgeMeter`, and `AeoKpi` over `AeoTracker`.
