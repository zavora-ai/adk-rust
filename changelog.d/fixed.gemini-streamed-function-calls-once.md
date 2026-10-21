- `GeminiModel` yields each function call of a streamed turn once, on the final chunk. A call Gemini
  sent on a chunk without `finishReason` was yielded on a partial chunk and again in the terminal
  event the agent persists, so anything counting calls over events saw it twice (spec 3.1 §7.13).
  Text on partial chunks still streams as it arrives; the Interactions transport already flushed
  calls on completion.
