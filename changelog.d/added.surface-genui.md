- **Agent-drawn UI** (`adk-surface`, `adk-cli`): the `genui` feature gives an agent render
  tools (`DrawToolset`: `render_form`, `render_card`, `render_table`, `render_screen`,
  `patch_surface`, `delete_surface`) that draw A2UI v0.9.1 surfaces on the `forms` 1.0
  catalog in a person's thread. Every drawing is checked on the server on the strings a
  person reads (`Validator`): no decision's words or approval controls, no secret fields, no
  links, catalog components and declared actions only, each bound to one of the drawing
  agent's own tools (`ToolContext::agent_tool_names`); a refused drawing is a tool error for
  the agent and a plain notice in the thread. Only a render tool's result, proven by the
  `adk.surface.drawn` state record, is drawn. A form's **Send** (`Command::SubmitDrawn`)
  checks the version seen, the action and each value, waits for the thread's running turn,
  records `surface.drawn.submitted` with digests and never the values, and
  asks the drawing agent for the call (`Source::request_call`), which it holds for the person
  in its own kernel target; the card starts "You pressed Send on a form Finance drew" and
  never offers **Always allow**. The web home renders drawings and the developer panel's
  Protocols tab reads `GET /api/surface/genui/capabilities`; the terminal draws a box and
  fills a form with ctrl-f; `--output json` adds `drawn`, `drawn.submitted`,
  `drawn.refused`, `app` and `app.asked`. `adk-cli`'s `genui` feature gives every harness `adk-rust serve` and `up`
  host the render tools (spec 3.3 §6.1, §7.4 rule 5). See
  `docs/official_docs/surfaces/genui.md`.
- **Apps from MCP servers and drawings over AG-UI** (`adk-surface`, `adk-cli`): the
  `genui-apps` feature shows an MCP server's app in a thread under "App from Weather · it
  can't approve anything" (`McpAppsHost`): framed sandboxed on its own origin under a CSP
  built from what it declares and framed only by the home's page, blocked when it reaches
  this computer, its network or the home, loads undeclared sites, or shows a decision's
  words, and refused on the surface API whatever `SecurityConfig` admits
  (`refuse_apps_origin`, `refuse_apps_origin_recorded`). An app's `tools/call` is held as the
  agent whose call showed it, by that tool's own name, for the person ("The app from Weather
  asked", never **Always allow**), and answered once the agent has held or refused it;
  model-only, app-only and other servers' tools are refused, and an app with three calls
  waiting asks for no more; `ui/message` waits for **Send it** and is sent marked as the
  app's; `ui/open-link` waits for **Open** and never opens this computer or its network.
  `GenuiAgUi` sends drawings as AG-UI `a2ui-surface` activities, drops echoed ones that fail
  the validator, and holds a form sent through `forwardedProps.a2uiAction` when the run's
  agent drew it. The Protocols tab claims AG-UI and MCP Apps only where a host wired them
  (`router_wired`, `GenuiWiring`). `adk-cli`'s
  `genui-apps` feature serves apps on `adk-rust serve` and `up` from a second loopback
  listener, or `--apps-origin`. The renderer is published as `@zavora-ai/adk-surface-react`
  (`adk-surface/react`) (spec 3.3 §6.1, decision 11). See
  `docs/official_docs/surfaces/genui.md`.
