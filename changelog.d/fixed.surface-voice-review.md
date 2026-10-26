- **Voice conversations stop, hold and resume as typed runs do** (`adk-surface`,
  `adk-harness`; spec 3.3 §5.6, S-I10): **Stop all** ends a live voice conversation and its
  calls check the harness runner's kill switch, resumes included; a conversation opened while
  every agent is stopped ends at once. A conversation has every tool the harness's model has,
  not only the harness toolset. An approval recorded while a conversation ends still runs its
  call. After a barge-in the transport flushed, speech counts again at once instead of after
  the interrupted reply's length. A transcript that arrives after the next utterance started
  is bound to its own utterance. A spoken "allow once" cancelled or stopped as its window
  ends is never committed. A call shown cut short is listed for the screen and prompted with
  "Say "deny", or decide on screen". The voice view shows the prompted call in full (§7.1),
  and an ended conversation opens in Threads. The terminal's voice line names the decision
  prompted and the one counting down, says how a spoken answer ended, says "spends money" for
  a money call, prompts only the voice session's own decisions, and drops queued audio when
  the person speaks over the model. `VoiceHub` conversations record `harness.voice.held` and
  `harness.voice.resumed`.
