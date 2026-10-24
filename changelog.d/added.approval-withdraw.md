- **Withdrawing a held call's approval request** (`adk-core`; spec 3.3 §7.3):
  `ApprovalService::withdraw` closes a pending request whose call was decided another way
  without writing an outcome to the store or waking its run (the default cancels it, and
  `StoreApprovalService` writes nothing), and `adk_core::withdraw_held_call` withdraws a held
  call's open requests and clears its pending entry.
