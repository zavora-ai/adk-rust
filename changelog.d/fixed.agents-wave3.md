- **Agents a person adds: suggestions, hosting and the record** (`adk-kernel`): a suggestion made
  again after a refusal gets a decision of its own; one whose decision cannot be written is taken
  back; a pause or resume keeps the agent's build, and a replaced, removed or stopped build is
  closed (`HostedClose`); an older build never replaces a newer one; a change that loses its write
  ends no decision; a kernel without areas checks agents at the mode it started in
  (`Agents::area`); a suggestion is checked against its own area's primary agent
  (`AgentFactory::ceiling_in`) and the suggester's limits in its turn (`AgentProposer::ceiling`);
  `Agents::answered` adds an approved agent as the surface that approved it; the listing carries
  removed agents; area refusals are in dollars; no agent gets the id `new`.
- **Your agents on the surfaces** (`adk-surface`, `adk-cli`): **Removed agents** at the roster's
  foot opens a removed agent's threads read only (`AgentsResponse::removed`,
  `SurfaceEngine::removed_agents`); the web home adds or widens an agent at high risk after
  "Confirm it's you" (`POST /api/surface/agents/confirm/options`, `/confirm`, feature `mobile`)
  and its settings show the agent's own rules and routines; agents sit under their area in the
  roster; a change refused as out of date names where it was made; the terminal's form edits
  against the version it read, `c` is **Change it first** on a suggested agent's card, `p`
  resumes a paused agent that cannot start, and `--output json` writes a line for any change;
  `/agent` takes `--version` and `--replaces` and previews each change as "Was … Now …", which
  `adk-rust agents` prints too.
