- **Drawings as Slack and Teams cards** (`adk-surface`, feature `genui`): `slack_blocks` and
  `adaptive_card` write a projected drawing as Slack Block Kit and a Teams Adaptive Card 1.5
  (text, the home's summary of charts and scenes, fields, unstyled buttons with the engine's
  line under each; no markdown), and `submit_from_slack` / `submit_from_teams` read a press
  back as the `SubmitDrawn` command the home sends, which the engine holds for the person's
  decision. The chat adapters do not post drawings yet.
