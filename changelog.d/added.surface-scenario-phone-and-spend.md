- **The phase 1 and 2 scenarios run the phone and spend money** (`adk-cli`): `PhoneSetup::with_push_allowlist`
  chooses the push services phones may subscribe through, which the hidden `adk-rust up --push-to-localhost`
  (builds with `mobile` and `scripted-model`) sets to a fake push service on loopback; each `--model-script`
  reply may name its price (`"cost"` in USD). `surface_approve_tomorrow` pairs a phone, records every wake-up
  and reads the phone's card; `surface_morning_after` spends on every scripted reply and checks the morning
  briefing waits for quiet time to end (spec 3.3 §12 phases 1 and 2).
