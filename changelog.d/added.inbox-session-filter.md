- **Approval inbox by session** (`adk-core`, `adk-session`, `adk-server`; spec 3.3 §10 C1):
  `InboxQuery::with_session` and `ApprovalRequestQuery::with_session` list the decisions one
  session — its application, user and session ID — waits on; `with_user_id` and
  `with_session_id` set the parts alone. `GET /api/approvals` takes `user` and `session`;
  `session` needs `app`, names a session of `user` (the caller by default), and never widens
  the caller's inbox. Every approval store filters in the store: the SQL stores add
  migrations 3 and 4 (indexed `session_id` and `user_id` columns on
  `adk_approval_requests`), and the SQL, MongoDB and Neo4j stores' `migrate` fills both for
  requests stored before them. The conformance suites gain `requests_list_by_session`,
  `inbox_lists_one_session` and `a_call_held_again_keeps_its_open_request`.
