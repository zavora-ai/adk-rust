- **Appending to a missing session is a typed `session.not_found`** (`adk-session`): every
  backend's `append_event` and `append_event_for_identity` report a missing session as
  `NotFound` instead of an internal `session.legacy` error, and an ID that more than one app
  or user holds is `session.ambiguous_id` on the in-memory backend too.
