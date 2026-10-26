- **Voice tools, kill switches and terminal hooks** (`adk-harness`, `adk-surface`; spec 3.3
  §5.6): `HarnessAgent::voice_tools` lists the tools the harness's model has beside its
  toolset, for `VoiceConfig::with_tools`; `VoiceConfig::with_governance` adds a kill switch
  every call of the realtime session checks; `VoiceSurface::record_held` and
  `VoiceSurface::resume_held_calls` record the hold and resumed records for a caller that
  reads the runner's events itself. `VoiceHub::open_in_thread` opens a conversation that
  prompts only its own session's decisions; `TerminalVoice::with_flush` drops the speaker's
  queued audio on a barge-in; `VoiceModels::agent_name` names the agent whose thread lists a
  conversation.
