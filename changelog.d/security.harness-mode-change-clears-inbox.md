- **A mode loosening leaves nothing to approve later** (`adk-harness`): `Harness::set_mode`
  removes the pending entry of every held call it cancels, and any decision already
  recorded for one, in the approval store and the approval service, so neither a later
  approval nor an earlier one authorizes a new call with the same fingerprint unasked.
  `Harness::decide` refuses a fingerprint that is not pending with
  `harness.decision.invalid` (spec 3.1 §7.4, §9.2 I14).
