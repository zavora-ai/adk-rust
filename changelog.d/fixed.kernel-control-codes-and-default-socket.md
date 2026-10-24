- **Kernel control-plane refusals keep their codes, and a default kernel serves no socket**
  (`adk-kernel`): `ControlClient` reported `kernel.agenda_not_found`, `kernel.agenda_state`,
  `kernel.quota`, `kernel.spawn_rejected` and `kernel.ledger.invalid_row` as
  `kernel.control_plane`; it now keeps every code the kernel answers with, and a test checks
  the list against every `*_CODE` constant in the crate. `KernelConfig::default()` no longer
  opens the control socket at the shared home, where a second kernel collided with the one
  `adk-rust up` runs; `with_control(ControlPlaneConfig::default())` serves it, as `adk-rust
  up` and the launcher's `up` do. Spec 3.2 §7.11.
