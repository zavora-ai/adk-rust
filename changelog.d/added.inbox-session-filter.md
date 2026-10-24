- **Approval inbox by session** (`adk-core`, `adk-session`, `adk-server`; spec 3.3 §10 C1):
  `InboxQuery::with_session_id` and `ApprovalRequestQuery::with_session_id` list the
  decisions one session waits on, and `GET /api/approvals?session=` exposes the filter. Every
  approval store filters in the store: the SQL stores add migration 3 (a `session_id` column
  and index on `adk_approval_requests`), and the SQL, MongoDB and Neo4j stores' `migrate`
  fills the session of requests stored before it. The conformance suites gain
  `requests_list_by_session` and `inbox_lists_one_session`.
