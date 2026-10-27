- **Every held call is announced on the kernel's bus** (`adk-cli`): `adk-rust up` publishes
  `KernelEvent::Held` for each request its inbox takes, so a call a voice conversation holds
  reaches the desktop tray and the terminal attached to the kernel on that event, not on their
  next timed read (spec 3.3 §12 phase 5 "the tray changes within one event").
