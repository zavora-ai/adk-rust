- **Your agents on every surface** (`adk-surface`): the roster lists agents a person added —
  **Ready** before they run, **Paused**, **Can't start** — with their own colours. The engine
  adds `newThread`, `addAgent`, `changeAgent`, `pauseAgent`, `resumeAgent`, `removeAgent` and
  `copyRuleToAgent` commands, only from a person at the web home or the terminal, and the
  outcomes `agentAdded` through `agentOutOfDate`. `GET /api/surface/agents/{id}`,
  `GET /api/surface/agents/choices` and `POST /api/surface/agents/preview` read and preview;
  `AgentsResponse::can_add` ends the roster with **New agent**. A `kernel:agent.create`
  decision is drawn with the kernel's heading and the definition in full
  (`DecisionCard::adds_agent`), answered **Add this agent** or **Don't add** (never Always
  allow), and the Telegram adapter offers **Don't add** only. The web home gains the New agent
  screen, agent settings and the card, which the phone's decide screen shows too; the terminal
  gains the Agents view (`Ctrl-G`, `/agents`), and `Tab` reaches an added agent with no thread
  yet. A removed agent leaves the roster; its threads stay. See
  `docs/official_docs/surfaces/agents.md`.
