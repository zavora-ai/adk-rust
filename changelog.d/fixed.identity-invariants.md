- **Workflow and team sub-agents act as themselves** (`adk-agent`): the sub-agents of a
  `SequentialAgent`, `ParallelAgent`, `LoopAgent`, conditional agent, or team now carry their
  own agent ids in policy requests, approvals, spend, and audit records instead of their
  parent's.
- **Self-approval spans subject kinds** (`adk-core`): `ApprovalRequest::is_self` treats a
  human and a service with the same id as one party, so `human:alice` cannot approve a call
  made by a scheduled run acting for `service:alice`.
