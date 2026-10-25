- **Sandboxed MSYS2 programs no longer share the user's MSYS2 namespace** (`adk-sandbox`,
  `adk-harness`): the restricted-token enforcer (`WindowsEnforcer::run_confined`,
  `spawn_contained`) runs an MSYS2 program — Git for Windows' `usr\bin\sh.exe`, or its
  `bin\sh.exe` launcher — from a private copy of `msys-2.0.dll` and the common interpreters,
  made for each call in an owner-only, Medium-integrity directory under
  `%LOCALAPPDATA%\adk-sandbox\msys2` and compared with the installation's files before use.
  Before, a sandboxed `sh` exited at start with `NtCreateDirectoryObject(...): 0xC0000022`
  while any of the user's MSYS2 programs ran, and, while none ran, created the user's
  namespace at Low integrity, so a Git Bash the user started next read a mount table the
  sandboxed command could write. Each call now also holds the user's namespace of the
  program's installation and of every installation on its `PATH` with an unconfined `cat`,
  so the token can neither write nor create it; an MSYS2 program that a program outside MSYS2
  starts itself, such as a hook's `sh` that `git` starts, therefore cannot start in the
  sandbox, and the harness reports that as a suspected denial. Installations off `PATH` stay
  unisolated:
  the `bash` declaration and `harness.bash.begin` carry `msys2Isolated` (`false` on Windows),
  `IsolationStatus::isolates_msys2` and the status line say so, and `auto_allow_sandboxed`
  stays refused.
