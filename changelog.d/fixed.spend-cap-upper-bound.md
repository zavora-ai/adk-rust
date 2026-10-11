- **Spend caps are hard caps** (`adk-core`, `adk-agent`, `adk-model`): a model call now
  reserves an upper bound on its cost — input bytes at the highest input-side rate plus
  the request's output cap, or the model's published output limit, at the output rate —
  instead of a per-call estimate, so a call that costs more than the estimate can no
  longer commit spend past a cap. A call the model cannot bound (unpriced model, no output
  cap and no published limit, `FileData` input) reserves `UNBOUNDED_HOLD_MICRO_USD`, which
  any covering cap refuses.
