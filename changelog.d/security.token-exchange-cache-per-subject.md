- **`TokenExchangeBroker` caches tokens per subject and delegation chain** (`adk-auth`): the
  cache was keyed by agent, resource, and scopes, so a token exchanged while the agent acted
  for one user was served to runs acting for any other user of the same agent. The key now
  also holds `ActingContext::on_behalf_of` and `delegation_chain`; one exchange is sent per
  subject and chain instead of per agent. `TokenExchangeBroker::on_behalf_of` adds an RFC 8693
  delegation mode: the token a new `SubjectTokenSource` supplies for the run's subject is the
  `subject_token` and the agent's credential is the `actor_token`, so the issued token carries
  the user's authority. A run acting for no subject fails with `credential.subject_missing`.
  The default agent-only request is unchanged (spec 3.1 §10 F6).
