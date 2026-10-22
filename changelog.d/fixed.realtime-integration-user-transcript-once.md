- **`IntegratedRealtimeRunner` saves an OpenAI user transcript once** (`adk-realtime`): when
  the user's partial transcripts arrived before the model's response started, the runner
  saved the partial text as a user turn at `response.created` and the completed transcript
  again when it arrived, and reported two `InputTranscriptCompleted` events. A transcript
  streamed under an item id now waits for its completion; one streamed without an item id
  (Gemini Live, which never completes one) still completes where the model's turn starts or
  ends.
