- **MCP Apps practice and opt-in tiered consent** (`adk-surface`, feature `genui-apps`):
  `ui/initialize` carries the page's style variables, the frame's width and maximum height,
  and the view state the app asked the host to keep (`ui/set-view-state`, up to 64 KB);
  `ui/request-display-mode` answers inline; the page sends `host-context-changed` when the
  scheme changes and `ui/resource-teardown` before the frame goes. `McpAppsHostBuilder::
  app_consent` (`AppConsent`, off by default) approves an app's calls to read-only tools, or to
  tools the person gave a standing approval while their definition is unchanged, as the
  person, recorded as `surface.app.call_auto_approved` with `initiator: "app"`
  (`AppServers::tool_definition`). A drawn form's Send is never approved this way.
