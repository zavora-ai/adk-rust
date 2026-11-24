- **One approval per previewed screen plan** (`adk-harness`, spec 3.7 Q8):
  `computer_use_preview { actions }` groups consecutive held desktop actions into screen plans
  of up to N actions in one window (`ComputerUsePack::with_plan_max_actions`, 10 by default),
  and `ApprovalPrompter::prompt_screen_plan` shows each plan on one card. The decision binds
  the plan's digest over its window, steps and previewed targets, answers each step once and
  in order with `plan: { digest, step }` added to its arguments, and is recorded as
  `harness.computer_use.plan_decided`. Before each step runs, the reference graph previews it
  again and the pack checks the fresh target against the previewed one; a deviation runs
  nothing (`harness.computer_use.plan_deviation`), ends the plan, and the step is held for a
  fresh approval. `computer_use_execute` stays outside every standing rule. Scenario S14 of
  `examples/vision` now takes one approval for the form's eleven steps and a second for the
  re-grounded Save.
