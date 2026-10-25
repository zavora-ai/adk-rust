- **The harness describes itself to the model** (`adk-harness`): `HarnessManifest`,
  generated when the harness is built from its tool classes in each mode, the surface's
  commands, members, packs, memory and what each safety mechanism covers, is rendered as a
  byte-stable `<harness>` block after `<environment>`; each request's reminder lists the
  standing rules that apply, deny first and numbered as `/rules` numbers them; and the
  `harness_help` tool (class `Read`) returns commands, a subcommand's long help, pages and
  the manifest on demand. `HarnessAgentBuilder::surface` and `help_page` describe the
  surface; `HarnessAgent::manifest` returns the manifest (spec 3.1 §7.25).
