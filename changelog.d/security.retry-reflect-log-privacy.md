- **Retry & Reflect keeps tool error text out of logs** (`adk-retry-reflect`): every
  retry, exhaustion, and skipped refusal or timeout logged the tool's error text, which can
  quote arguments and credentials. Log lines now carry the error's component, category, and
  code (`error`), and its text (`error.message`) only under the new
  `RetryReflectPluginBuilder::record_payloads(true)` (`RetryReflectConfig::record_payloads`,
  off by default). The reflection prompt the model receives is unchanged.
