- **`CustomAgent` runs runner plugin agent callbacks** (`adk-agent`): a runner
  `PluginManager`'s `before_agent` and `after_agent` callbacks now run ahead of a
  `CustomAgent`'s own, as they do for `LlmAgent` and `CodeActAgent`.
- **ACP resumes carry fingerprint approvals** (`adk-acp`): the permission bridge also
  records each decision by call fingerprint, so a resumed model that re-issues the call
  under a new ID receives the client's answer instead of a second permission request.
