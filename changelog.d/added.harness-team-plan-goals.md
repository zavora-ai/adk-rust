- **Harness team, plan graph and goals** (`adk-harness`, spec 3.1 §7.10–§7.12):
  - Members lowered to one `TeamSpec` with the harness `LlmAgent` as coordinator:
    `MemberSpec` (`explore` and `general` by default, `reviewer_acp`), `MemberKind::{Local,
    Acp, Remote, Agent}`, `member_registry` discovery through `compile_with_registry`,
    `team_policy`. Every edge is isolated (no history, state, artifacts or memory), timed and
    circuit-broken; delegation depth is 1; members inherit the session's mode and policy, and
    their state writes stay in their own session. ACP members' permission requests are
    decided on the governed path. `HarnessTeam::review_round` builds a round-robin
    `BlackboardSpec`. `harness.member.spawned` / `.finished` records and `harness.member`
    spans.
  - The plan graph: an approved `exit_plan_mode` with `steps` compiles to a checkpointed
    `StateGraph` (`compile_plan`, `PlanGraphShape::workflow_schema`); `approve` gates pause on
    a held `plan_step_gate` call bound to the plan hash and step and decided through
    `Harness::decide`; `review` gates run the reviewer member; `PlanGraphs::rewind` forks at
    the checkpoint after a step. `plan_checkpointer(..)` on the builder. `harness.plan.compiled`
    and `harness.plan.step` records.
  - Goals: `Harness::start_goal`, `Harness::goal`, `Harness::goal_target`,
    `Harness::with_run_store` (durable `harness.goal` runs), `Harness::ambient_goal`
    (feature `ambient`); `GoalEngine::{Loop, Graph}`, `UntilCheckAgent`, `GoalStore`
    (`InMemoryGoalStore`, `SqliteGoalStore`), `harness:goal` mirror, `harness.goal.iteration`
    records and `harness.goal` spans. `Harness::decide` wakes the durable run parked on the
    decided call.
  - `Harness::send` and `Harness::resume` answer, from the runner's action ledger, calls a
    stopped process left unanswered, so a completed call never runs twice after a crash.
