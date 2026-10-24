- **Windows restricted-token enforcer** (`adk-sandbox`, feature `sandbox-windows`): the
  Windows enforcer is now a restricted-token-plus-ACLs tier (spec 3.2 §10 W5) instead of an
  unimplemented AppContainer stub. `WindowsEnforcer::run_confined` runs a command under a
  restricted, Low-integrity token (privileges removed; every group deny-only except
  `Everyone`, `Users`, Authenticated Users, `INTERACTIVE`, the logon SID and integrity labels;
  restricting SIDs `Everyone`, `Users`, `RESTRICTED`, the logon SID and the user), grants
  `RESTRICTED` access to the policy's paths, labels the read-write ones Low, and holds the
  process in a Job Object that kills every descendant with the call. Writes outside the
  read-write paths are refused, except where a path is already labelled Low (such as
  `AppData\LocalLow`). Reads are allowed where the user may read: the user is a restricting
  SID because the MSYS2 runtime of Git for Windows' `sh` opens its signal pipe and shared
  memory through an ACL that names only the user. The grants and labels stay on the paths.
  Network access is not restricted. The enforcer's name is now `restricted-token`; `probe` and
  `configure_command` report that the generic `sandboxed_command` path cannot carry a
  restricted token, so a sandboxed shell built through that path stays degraded on Windows.
