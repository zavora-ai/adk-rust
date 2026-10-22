- **A `RealtimeAgent` run reports a session the provider refused** (`adk-realtime`): when the
  provider closes the socket with a failure code — Gemini Live closes with 1007 on a setup it
  does not support — the run ends with the error `model.realtime.session_closed`, carrying the
  close code and the provider's reason (category `InvalidInput` for a refused setup or
  message, `Unavailable` otherwise). The run used to end as though the conversation had
  finished. A normal close (1000, 1001) still ends the run without an error.
