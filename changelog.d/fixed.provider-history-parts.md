- **Preserve provider replay parts between invocations** (`adk-core`, `adk-agent`,
  `adk-runner`, `adk-model`): with a `provider_origin`, reconstruct opaque reasoning and
  signatures only for their captured provider, model and authorization owner. Without
  one, signed thinking, encrypted reasoning and thought signatures replay unchanged
  within an invocation, as before. Preserve visible text, tool history and original
  stored events without duplicating replay parts.
  Ownership uses namespaced JSON in the existing server-tool variant; the public
  `Part` enum remains unchanged. Ordinary calls keep their IDs and arguments, with
  signatures restored only for the matching owner. Internal records never reach
  provider requests or quoted context without projection.
