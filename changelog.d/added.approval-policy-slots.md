- **Approval slots and an approver directory** (`adk-core`): `ApprovalPolicy::slots`
  (`ApprovalPolicy::for_slots`, `with_slots`) needs one distinct approver per slot — an
  editor and a publisher who are two people — before a request resolves, whatever order they
  decide in; `ApprovalRequest::slot_holders` names who fills each slot and `open_slots`
  lists the slots still waiting.
  `StoreApprovalService::with_directory` takes approvers' and delegators' groups from an
  `ApproverDirectory` and ignores the groups callers assert; decisions record the groups
  their authority acted under in `ApprovalDecision::authority_groups`. Stored requests read
  back unchanged on every approval store, and the approval service conformance suite gains
  `slots_need_distinct_approvers` and `directory_decides_groups`, run on all seven backends.
