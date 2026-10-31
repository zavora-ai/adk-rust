- **Media calls inside a run count against its caps** (`adk-agent`, `adk-model`, `adk-audio`):
  Muse image and transcription calls, and `TranscribeTool` and `SpeakTool` calls to a billed
  `adk-audio` provider, reserve and commit spend and count against the run budget. The bundled
  `adk-audio` cloud providers carry no per-unit price, so under a budget cost cap or a
  spend-ledger limit their calls are refused before they are sent. Muse spend is keyed under the
  vendor `muse` instead of `unknown`. `MuseImagesClient::edit` sends the configured model rather
  than always `muse-image-1.0`.

  Migration: to run unpriced media calls under a budget cost cap, add
  `RunBudget::allow_unpriced_models()`; rename spend limits keyed on the vendor `unknown` for
  Muse calls to `muse`.
