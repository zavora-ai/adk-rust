- **The edge files its own signal for a hostile conversation** (`adk-server` `awp`): when the
  public partition's policy refused a tool call in a turn, or the new `ConversationScreen`
  (`InjectionScreen` by default, `AwpEdgeBuilder::conversation_screen`) flagged the message,
  and the public agent filed no signal, the edge files `abuse_observed` naming the verified
  caller, else `inbound_message` for the thread, with no text from the message (spec 3.5
  §7.10, S7).
- **Verified agent discovery is audited and counted** (`adk-server` `awp`, `adk-website`): a
  verified caller's fetch of the Agent Card, the manifest or the MCP Server Card is an
  `awp.request.verified` record with `awp.surface`; `EdgeMeter` counts `agent_discovery` and
  `agent_discovery:{surface}` for the `agents` KPI source (spec 3.5 §7.12).
- **`LeadStore::delete`** (`adk-awp`), implemented by the in-memory, SQLite, PostgreSQL and
  Redis stores and checked by the `leads_delete` conformance case; `EdgeEvents::retained`
  (`adk-server`) reads the edge's retained events for the site's own views (spec 3.5 §7.5).
