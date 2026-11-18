- **Microsoft Teams chat channel** (`adk-surface` feature `teams`, `adk-cli`): `TeamsAdapter`
  serves a bot registered with Azure Bot Service as a chat thread over the kernel's channel
  contract. Every Bot Framework activity is refused until its Bot Connector JWT passes each
  protocol check: `RS256` signature by a key from the connector's OpenID metadata (cached a
  day, refetched for an unknown key at most every five minutes), issuer, audience = app ID,
  validity with five minutes' skew, the key's `msteams` endorsement and the `serviceurl`
  claim. Replies and wake-ups go out through the Bot Connector REST API with a cached
  Microsoft Entra ID client-credentials token; cards are Adaptive Cards whose
  `Action.Execute` buttons (with an `Action.Submit` fallback) carry the decision and version,
  under the same rules as every channel. `adk-rust up` starts it when `TEAMS_APP_ID` and
  `TEAMS_APP_PASSWORD` are set, serving `POST /api/messages` on a listener of its own
  (`TEAMS_BIND`, `127.0.0.1:3978`) so publishing it never publishes the home.
