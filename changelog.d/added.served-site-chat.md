- **Chat on a served site** (`adk-website`, `adk-kernel`, `adk-surface`, `adk-cli`; spec 3.5
  §10 X12, §7.19, §5 S18): `SiteOptions::chat` takes the site's chat platforms
  (`deploy::chat::ChatAdapters`), and the served site runs one channel gateway on its kernel
  with a route of its own (`deploy::chat::SiteChat`): a paired account's message is a turn of
  the Steward entering through its device, a stranger's goes to the Inbox with no pairing code
  and no turn unless they type `/pair`, and the photos a paired person sends join the draft
  their turn makes through `channel::OwnerChannel`. The edge serves WhatsApp's webhook at
  `/channels/whatsapp`. `adk-rust website new` builds the adapters from the project's
  `channels.json` (`deploy::config::chat`), its secrets `env:` or `file:` references;
  `SiteStores` keeps chat pairings (`kernel.channel_bindings`) and `SiteStores::open_with` takes
  approval notifiers. The kernel's gateway takes an `InboundRoute`
  (`ChannelGatewayBuilder::route`, `InboundOutcome::Routed`), `ChannelInbound` carries
  `attachments` (`ChannelAttachment`), and a paired message with no words starts no turn
  (`InboundOutcome::Empty`). The WhatsApp and Telegram adapters download a message's images,
  at most 10 MiB each, and a Telegram photo's caption reaches the gateway as the message's
  words; Slack and Discord take `/pair` as a command. `SiteRuntime::newest_draft_from` finds
  the draft a paired device's turn made.
