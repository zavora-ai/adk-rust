- **Managed Agents defaults deny network egress** (`adk-anthropic`):
  `CreateEnvironmentParams::cloud` creates `limited` networking with no allowed hosts,
  package registries, or MCP servers; `cloud_limited(name, hosts)` allows listed hosts and
  `cloud_unrestricted(name)` opts in to full egress. `ToolConfig::agent_toolset` and
  `agent_toolset_with_policy` disable `web_fetch` and `web_search`, which run outside the
  environment's networking policy; `ToolConfig::agent_toolset_with_web` enables all eight
  tools.
