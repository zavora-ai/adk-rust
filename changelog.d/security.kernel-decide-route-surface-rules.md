- **The approvals route on a kernel answers under the caller's surface** (`adk-kernel`,
  `adk-server`, `adk-cli`; spec 3.3 §7.2, S-I11, S-I13): `POST /api/approvals/{id}/decide`
  checks each decision for the surface the server established — the kernel's socket or pipe
  as its own terminal, a paired device by its kind, an operator credential over HTTP as `api`
  — and refuses what that surface may not give with `403 surface.decide.not_here`: a tray
  edits no call and adds no agent, and only the kernel's own terminal approves a high-risk
  call there. An admitted decision is recorded as `approvals.decide` with its surface, how and
  transport before it reaches the inbox, and a suggested agent it answers is added at once as
  that surface. `ApprovalEndpoints::with_decide_guard` and
  `ServerBuilder::with_approval_endpoints` install such a check on any server.
