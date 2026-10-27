- **Screen actions** (`adk-surface` feature `verb-ui`, Experimental; `adk-cli` feature
  `verb-ui`; spec 3.3 §2 item 7, §5.6): people and agents share one way to change something a
  surface shows, starting with **Snooze** in Coming up — an hour later or the same time
  tomorrow. A person snoozes from the web home, the phone, the terminal (`/snooze <title>
  [tomorrow]`) or by voice ("Snooze news digest", after the engine's cancel window), through
  `authorize_tool_call` under `urn:adk:agent:<app>/surface.verbs`; it still runs while
  everything is stopped, since it only delays work. An agent asks through `verb_snooze`
  (`VerbToolset`, origin `verbs:<app>`), and `VerbPolicy` holds every such call for a person
  whatever the mode or standing rules, at most three waiting per agent; its card reads "Move
  News digest to 19:00", is low risk, offers Allow once · Edit · Deny and never **Always
  allow**. Chat channels and the tray take no screen action. Every action writes a
  `surface.verb` record. `VerbRegistry`, `VerbGate`, `VerbRouter`, `VerbHints`,
  `web::verbs::router` (`/api/surface/verbs`), `VoiceUpdate::PendingAction`,
  `TelegramAdapterBuilder::card_hints`, and `adk-rust up` hosting them for every agent it runs.
