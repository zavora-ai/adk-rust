- **A denied screen action names its item on record** (`adk-surface`, `adk-kernel`,
  `adk-cli`): "What your agents did" reads "Finance asked to snooze News digest; you said
  no. Nothing changed." instead of "something in Coming up". The `surface.decide` record of
  an answer to an agent's screen action — from the home, the phone, the terminal, voice or
  chat — carries the item's `title` and the card's `summary`, named by the new provided
  `CardHints::names`, and the kernel's `RecordLine` carries `title` (spec 3.3 §8, §11).
  A denied screen action shows that one line: the governed path's own record of the call is
  told by its publisher (`RecordLine::tool_source`, `policy_ref`) and never shown by the
  tool's name, such as "Finance was not allowed to use verb_snooze".
