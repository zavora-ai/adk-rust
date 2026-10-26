- **Card hints and granted uses** (`adk-surface`; spec 3.3 §5.7, §7.1, §7.2): a feature that
  publishes its own tools gives their cards a heading, a chat heading, "You can change it
  back" (`Reversibility::CanBeChangedBack`), a first step of "How it got here" and a line under
  the details (`DecisionCard::detail`, `CardDetail`) through `CardHints`, registered with
  `SurfaceEngineBuilder::card_hints` and read wherever the engine draws a card. A hint may mark
  money or take **Always allow** away; it never adds an answer or changes the risk.
  `SurfaceEngineBuilder::grant(GrantedUse)` lists what a host grants every agent it runs on
  the agent's detail (`AgentDetail::granted`), shown in the web home and the terminal as "On
  this home it can also:". The web home reads each feature's routes through one run-time probe
  (`web/src/features.ts`), which the website's tab now uses.
