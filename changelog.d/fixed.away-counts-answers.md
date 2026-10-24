- **"While you were away" counts the person's answers** (`adk-kernel`; spec 3.3 §10 K4, §12
  phase 2): an answer to a held call is a `decided` line, read from the `approval` or
  `rejection` row its audit record writes to the `/proc` ledger, with `approve` or `deny` as
  its effect. The kernel writes no `decided` row of its own, so no answer was counted before.
