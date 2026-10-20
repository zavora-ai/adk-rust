- **A freeze resolves an open approval prompt** (`adk-harness`): while a person is
  deciding a held call, `PromptingConfirmationHandler` watches the run's governance
  controls every 250 ms; when one is frozen the prompt is abandoned, the call is not run
  and the turn ends with `governance.frozen`, as it does when a freeze lands after an
  approval. Before, an open prompt waited for the person or the confirmation timeout
  (spec 3.1 §9.2 I20).
