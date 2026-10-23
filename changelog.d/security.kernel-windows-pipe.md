- **The kernel's named pipe and home files admit only their owner on Windows** (`adk-kernel`,
  `adk-cli`; spec 3.2 §10 W2, W4): the control plane's pipe carries a protected DACL whose
  one entry grants the user the kernel runs as, who also owns it, full access; a connection is
  admitted only when the token the client opened it with is that user's (identification-level
  impersonation after the first message), and the client refuses a pipe another account owns.
  The first instance is created with `FILE_FLAG_FIRST_PIPE_INSTANCE` and remote clients are
  rejected. The kernel home's `kernel-id`, `kernel-audit.key` and the service's `kernel.log` are
  created with an owner-only DACL, as Unix creates them `0600`.
