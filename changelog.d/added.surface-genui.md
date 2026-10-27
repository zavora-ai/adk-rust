- **Agent-drawn UI** (`adk-surface`, `adk-cli`): the `genui` feature gives an agent render
  tools (`DrawToolset`: `render_form`, `render_card`, `render_table`, `render_screen`,
  `patch_surface`, `delete_surface`) that draw A2UI v0.9.1 surfaces on the `forms` 1.0
  catalog in a person's thread. Every drawing is checked on the server on the strings a
  person reads (`Validator`): no decision's words or approval controls, no secret fields, no
  links, catalog components and declared actions only; a refused drawing is a tool error for
  the agent and a plain notice in the thread. Only a render tool's result, proven by the
  `adk.surface.drawn` state record, is drawn. A form's **Send** (`Command::SubmitDrawn`)
  checks the version seen, the action and each value, records `surface.drawn.submitted`, and
  asks the drawing agent for the call (`Source::request_call`), which it holds for the person
  in its own kernel target; the card starts "You pressed Send on a form Finance drew" and
  never offers **Always allow**. The web home renders drawings and the developer panel's
  Protocols tab reads `GET /api/surface/genui/capabilities`; the terminal draws a box and
  fills a form with ctrl-f; `--output json` adds `drawn`, `drawn.submitted` and
  `drawn.refused`. `adk-cli`'s `genui` feature gives every harness `adk-rust serve` and `up`
  host the render tools (spec 3.3 §6.1, §7.4 rule 5). See
  `docs/official_docs/surfaces/genui.md`.
