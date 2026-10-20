- **Enhanced plugins get a `PluginContext` per invocation** (`adk-plugin`): one
  `EnhancedPluginManager` serves every run of its agent and handed every hook the same
  context, so one user's run read and overwrote the plugin state of another user's
  concurrent run. Each invocation (app, user, session, and invocation id) now gets its own
  context (`EnhancedPluginManager::invocation_context`), and state meant for every invocation
  lives in the per-agent context, reached from a hook with the new `PluginContext::shared`
  (`EnhancedPluginManager::context` still returns it). `PluginContext::with_shared` builds an
  invocation context for testing a plugin outside a manager. A plugin that counted across
  runs in the hook's context now counts per invocation; move that state to
  `plugin_ctx.shared()` (spec 3.1 §12 phase 0).
