- **A driver's `publish_public` or `financial` call needs "Confirm it's you" on every
  surface** (`adk-surface`): the engine rated a call by its request's risk and the
  `payments_*` names only, so a paired phone or tray approved such a call with one tap
  through `POST /api/surface/decisions/{id}/answer`, which the kernel's own decide route
  refuses. A source now rates a call too (`Source::rates_high`, `Source::moves_money`);
  `LocalSource` over a kernel rates a tool its mounted drivers declare `publish_public` or
  `financial` high risk, and `financial` as moving money (spec 3.3 §7.2, spec 3.2 K-I9).
- **Only the web home and a paired phone take over** (`adk-surface`): the take-over route
  took its surface from the request body, so a remote terminal or a tray approved a
  take-over entry recorded as the web home. A device now takes over as its kind's surface,
  and a terminal or tray device gets `403 surface.not_here` (spec 3.3 §8.2).
