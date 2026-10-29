- **Photos from Slack, Discord and Matrix** (`adk-surface`; spec 3.5 §10 X12, §5 S18): the
  three adapters download a paired account's images into `ChannelInbound::attachments`, under
  the same bounds as WhatsApp and Telegram (JPEG, PNG, WebP and GIF, at most 10 MiB each and
  ten a message, checked while streaming). Slack takes a `file_share` message's `files` from
  `url_private_download` with the bot token, sent to `files.slack.com` only; Discord takes a
  message's `attachments` from its CDN without the token; Matrix takes `m.image` through the
  authenticated media endpoint, falling back to the legacy one. No adapter downloads a
  stranger's files, WhatsApp's and Telegram's included.
