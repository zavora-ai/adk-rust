- Preserve scoped Windows sandbox writes while allowing system runtime reads. The token
  makes its user SID deny-only and checks restricting SIDs for writes, with mode-limited
  `RESTRICTED` and logon grants on policy paths. Public `Users` or `Everyone` write ACEs
  alone do not authorize external writes; owner-only private reads and DACL changes stay
  denied. Public runtime reads and existing persistent path grants remain documented.
