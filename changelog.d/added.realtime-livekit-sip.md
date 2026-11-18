- **LiveKit SIP for realtime voice agents** (`adk-realtime`, feature `livekit-sip`):
  `telephony::livekit::LiveKitSip` creates inbound and outbound trunks and per-caller dispatch
  rules, `LiveKitWebhookVerifier` turns a signed `participant_joined` for an inbound SIP
  participant into an `IncomingCall`, `LiveKitSipCall` transfers (`TransferSIPParticipant`) and
  hangs up (`RemoveParticipant`), and `LiveKitSipDialer` places calls (`CreateSIPParticipant`,
  waiting for the answer). The feature uses LiveKit's server API only, so any realtime provider
  the LiveKit bridge puts in the room takes the call.
