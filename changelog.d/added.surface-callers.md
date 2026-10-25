- **Device and voice callers on the surface API** (`adk-surface`): `Caller` carries the
  paired device it acts from (`Caller::device`, `with_device`), and every `surface.*` audit
  record names it, with `presence: "confirmed"` when the person confirmed it is them. A
  device token's context is never the web home and never a paired local terminal, and
  `POST /api/surface/commands` answers it `403 surface.not_here`; a `decide` declaring the
  voice surface or origin answers `400 surface.invalid` (spec 3.3 §7.2, §10 K2, §11 S-I10).
