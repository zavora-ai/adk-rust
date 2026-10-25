- **The terminal says where a decision was answered** (`adk-surface`, `adk-cli`; spec 3.3
  §5.2, §12 phase 1): `adk-rust --output json` attached without a message writes a `decided`
  line for each decision of the session answered before, and one for each decision answered
  elsewhere while it runs, in the words every other surface shows ("You said yes in Telegram
  at 07:31"). A terminal attached to a kernel reads how each answer was given from the
  kernel's record through the new `RecordAnswerLog`.
