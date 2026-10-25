- **A contained command's output ends when the command exits** (`adk-sandbox`, `adk-harness`):
  `WindowsEnforcer::spawn_contained` made the child's three pipe ends inheritable while it
  created the child, so a process another thread started at that moment through
  `std::process::Command`, which passes on every inheritable handle, took them and held the
  command's output open until it exited — for a long-lived process such as an MCP server or
  an MSYS2 keeper, until the `bash` call's timeout. The pipe ends are now never inheritable in
  the calling process: their inheritable copies are made in a broker, a suspended `cmd.exe`
  from the system directory in the call's Job Object that never runs, which the child names as
  its parent and inherits exactly those three handles from. The child's parent process id is
  therefore that of an exited process. The unconfined `cat` that holds an MSYS2 namespace starts
  the same way, so it no longer takes another call's handles either.
