- **Media calls are metered by unit** (`adk-core`, `adk-model`, `adk-audio`, `adk-agent`,
  `adk-realtime`): `UsageUnit` and `UsageVector` count usage per unit (tokens, images, audio and
  video milliseconds, characters, pages, requests) and `UnitRate` prices a unit in micro-USD.
  `ModelCallScope::meter` and `meter_usage` meter a call billed in units — the spend hold covers
  `UsageCall::max_cost_micro_usd` before the call is sent, `MeteredUsage` commits its cost, and
  the run budget counts the cost, tokens and units (`BudgetTracker::units`); a call with no known
  price is refused under a cost cap with `budget.cost_unknown`. `ModelSpendHold::observe_cost`
  settles a hold without an `LlmResponse`. `PricingCatalog::unit_cost_micro_usd` prices
  `muse-image-1.0` ($0.01 per image), `muse-voice-transcribe-1.0` ($0.18 per audio hour, whole
  seconds rounded down) and `sam-3.1` ($2.50 per 1,000 images). `MuseImagesClient` and
  `MuseAsrClient` meter every call against the current run. `SttProvider::billing` and
  `TtsProvider::billing` (`AudioBilling`) name how a provider bills, and `TranscribeTool` and
  `SpeakTool` meter billed calls. `LlmAgent`, `CodeActAgent` and the realtime runtimes run every
  tool call with the run's scope current (`ModelCallScope::within_tool`), so media cost reaches
  the spend ledger, the run budget and the kernel's partition totals.
