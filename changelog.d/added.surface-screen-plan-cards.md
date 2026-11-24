- **Screen plans on decision cards** (`adk-surface`, `adk-harness`, spec 3.7 Q8): with
  adk-surface's new `computer-use` feature, a previewed screen plan whose first step parks shows
  as one decision card on the web home, the phone, the terminal and chat — its window, every
  step with its target in points, and the screenshot each target was found on
  (`DecisionCard::plan`, `ScreenPlanView`). **Allow this plan** decides every step through
  `Harness::decide_held_screen_plan` with the live prompter's binding, `plan: { digest, step }`,
  and its `harness.computer_use.plan_decided` record (now with `approvalId` and the person as
  the actor); the later steps' decisions are filed in the approval store under each step's
  fingerprint and taken back when the plan ends or is previewed again. A step whose target
  moved is held again on a fresh card; a plan too long for the card is approved only once shown
  in full; chat, the tray and voice may only deny it; it offers no **Always allow** or **Edit**;
  and **Stop all** closes an open plan card, cancelling its held steps. `Source` gains
  `screen_plan`, `decide_screen_plan` and `close_screen_plan`, implemented by `LocalSource` with
  a harness; `Harness::held_screen_plan` tells which plan a parked call is a step of.
