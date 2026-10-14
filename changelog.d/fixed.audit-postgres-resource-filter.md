- **Literal resource filter in the PostgreSQL audit sink** (`adk-auth`):
  `AuditFilter::resource` matches a literal substring, as the in-memory filter does.
  `%` and `_` in the filter were `LIKE` wildcards, so `delete_file` also matched
  `deleteXfile`.
