- **Realtime usage is metered** (`adk-realtime`): `RealtimeUsage::from_response` reads
  OpenAI's `response.usage` and Gemini Live's `usageMetadata` from a `ResponseDone`,
  `RealtimePricing` prices it, and `RealtimeRunner`, `IntegratedRealtimeRunner` and
  `RealtimeAgent` reserve a hold for each response in the run's `SpendLedger` and commit
  the priced cost — or the run's `LlmSpendEstimate` without pricing — when it ends; a
  refused hold ends the session with `RealtimeError::SpendRefused`. `RealtimeAgent` yields
  the usage on an event at each response's end. The Gemini session keeps a usage report
  sent on its own for the response that closes next.
