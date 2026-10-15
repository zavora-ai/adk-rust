- **Streamed responses persist as whole parts** (`adk-agent`): `LlmAgent` joins consecutive
  text deltas into one text part and consecutive thinking deltas into one thinking part, so the
  terminal event and session history no longer hold one part per delta. A thinking part closes
  when it is signed. Tool-call markup split across deltas is parsed once the text is complete,
  so the call is executed instead of persisting as text.
