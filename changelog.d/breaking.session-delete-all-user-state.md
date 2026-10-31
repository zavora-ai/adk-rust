- **`delete_all_sessions` also deletes the user's `user:` state** (`adk-session`): values a
  caller relied on outliving the user's sessions belong in `app:` state or outside the session
  store. `SessionService::delete_all_sessions`' default implementation returns
  `session.delete_all.unsupported` (`Unsupported`) instead of an internal `session.legacy`
  error.
