- **An origin per MCP server, and what an app's frame is granted** (`adk-surface`, feature
  `genui-apps`): with a `localhost` apps origin, each server's apps are framed on an origin of
  their own under it (`McpAppsHost::server_origin`, `McpAppsHostBuilder::origin_per_server`),
  served only there; the home's `frame-src` names `*.localhost:<port>` and the surface API
  refuses requests from every server's origin. `ui/initialize` reports what the frame was
  granted in `hostCapabilities.sandbox`. `ui/download-file` shows "The Weather app wants to
  save forecast.csv (2 KB)" with **Save** and **Cancel** (`AppPrompt::Download`): an embedded
  resource up to 5 MB, its name sanitized, never a file that runs or opens as a page.
