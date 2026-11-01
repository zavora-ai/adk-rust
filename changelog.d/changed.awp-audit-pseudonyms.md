- **AWP edge audit records name visitors by pseudonym** (`adk-server`): the edge wraps the
  recorder of its `audit` trail in `PseudonymousRecorder` over its subject registry, so records
  carry `psn:…` instead of a visitor id, and erasing a visitor deletes the pseudonym.
