- **Resume all only from the web home or the kernel's own terminal** (`adk-kernel`,
  `adk-surface`; spec 3.3 S-I10): `POST /api/kernel/unfreeze` over HTTP answers
  `403 kernel.resume.not_here` whoever holds the operator credential, so `adk-rust unfreeze
  --server` no longer resumes a kernel; the socket and the named pipe still do. On
  `POST /api/surface/commands`, **Resume all** and resuming an agent are the web home's only
  from the home's own page (a browser's same-origin request); from any other client they read
  as a terminal elsewhere and are refused with `403 surface.not_here`.
