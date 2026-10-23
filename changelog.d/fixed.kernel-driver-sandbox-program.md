- **A sandboxed driver whose program lives outside the system directories starts**
  (`adk-kernel`): under `sandbox = "os"` the launcher runs the file the command resolves
  to, through any link, and opens read-only its directory, a script's `#!` interpreter (and
  what `#!/usr/bin/env` finds), and an ELF program's dynamic loader and `RUNPATH`
  directories, with `/nix/store` for a Nix-built program. Bubblewrap previously failed with
  `execvp …: No such file or directory` for a linked program, a script whose interpreter is
  elsewhere, or a binary built against the Nix store's loader. `DriverIsolation::OsSandbox`
  records the directories as `program_paths`. Spec 3.2 §7.7.
