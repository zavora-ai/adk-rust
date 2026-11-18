- **Firestore session writes survive contention** (`adk-session`, feature `firestore`): a
  session transaction Firestore aborts, such as a `delete_all_sessions` racing other writes,
  runs again from its reads with backoff for up to two minutes, as Firestore's client
  libraries do, and `FirestoreSubjectRegistry::ensure` retries its create the same way.
  Failures Firestore reports as retryable now carry `session.firestore.retryable`
  (`ErrorCategory::Unavailable`) instead of `session.legacy`.
