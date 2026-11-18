- **Phone calls for realtime voice agents** (`adk-realtime`, features `telephony` and
  `openai-sip`): `CallControl`, `CallAnswerer` and `OutboundDialer` control calls on any
  provider; `TransferCallTool`, `EndCallTool`, `SendDtmfTool` and `PlaceCallTool` expose them to
  the model as governed tools that reach only destinations the application names, with
  `place_call` capped per tool; `hang_up_on_freeze` ends a call when a kill switch freezes.
  `openai::sip` verifies `realtime.call.incoming` webhooks (Standard Webhooks, with a replay
  guard), accepts, rejects, transfers (SIP REFER) and hangs up calls, and
  `OpenAIRealtimeModel::with_call_id` attaches a session to a call. See
  [Telephony](docs/official_docs/realtime/telephony.md).
