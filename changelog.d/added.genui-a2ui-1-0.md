- **A2UI 1.0 on the wire** (`adk-surface`, feature `genui`): drawings are written in A2UI 1.0
  by default (`@path`/`@call` bindings, `@accessibility`/`@checks`, one `createSurface`
  carrying components and data model, `@catalogId` on extension components) and in v0.9.1 for
  renderers that offer nothing newer (`A2uiVersion`, `to_wire`, `from_wire`,
  `GenuiAgUi::with_a2ui_version`). `render_screen`, `patch_surface` and echoed AG-UI
  activities read either syntax; reading 1.0 refuses what would change meaning in the
  internal form. `A2uiCapabilities` lists `1.0` beside `v0.9`, the developer panel claims 1.0
  supported and 0.9.1 legacy, and the web package's parser reads both.
- **Rule 5 in A2UI 1.0 terms** (`adk-surface`, feature `genui`): accessibility labels are
  checked like any shown text; a button's `userMessage`, a field or button hidden from screen
  readers, and an echoed surface asking for `sendDataModel` are refused. A refusal carries
  A2UI 1.0's `ValidationResult` (`ValidationResult`, `Severity`) after its plain words.
