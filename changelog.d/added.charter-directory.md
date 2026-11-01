- **The charter as the approval service's directory** (`adk-kernel`, `adk-core`):
  `charter::CharterDirectory` gives a person the `role:{role}` groups the site charter in
  force names — the draft's own for a site's first charter — and none to an agent;
  `chain_policy` turns a charter's approval chain into an `ApprovalPolicy` with one slot per
  role, and `role_group` and `group_role` map between roles and groups.
  `ApproverDirectory::governs` names the groups a directory decides, so a service with a
  `CharterDirectory` ignores claims to `role:` groups and keeps other asserted groups, such
  as an operator scope.
