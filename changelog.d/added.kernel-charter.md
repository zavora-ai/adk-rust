- **Charter, KPIs, signals and earned autonomy in the kernel** (`adk-kernel`, feature
  `charter`, spec 3.5 §7.10, §7.11): `CharterStore` keeps every AWP 2.0 charter version with
  the decision that committed it and refuses invalid drafts and approval chains not satisfied
  by distinct people; `KpiStore` and `KpiTracker` record KPI series with daily and weekly
  aggregates, keep the measure-only period before a baseline, and pause a job before its next
  action on a guardrail breach through `JobControl`; `AutonomyLadder` raises a job's autonomy
  only by an owner's decision and lowers it on breaches, rejections and incidents;
  `SignalSink` files typed signals as `AgendaOrigin::Signal` agenda items that wake
  `Wake::Signal` sleepers; `SiteProcessKind` names the site's process kinds. In-memory,
  SQLite, PostgreSQL and Redis backends with conformance suites.
