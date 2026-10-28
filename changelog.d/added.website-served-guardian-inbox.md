- **A served site runs the Guardian's probe and the Inbox, configured from the project**
  (`adk-website`, `adk-cli`, spec 3.5 §7.19, §7.20, §7.22, §10 X12): `deploy::serve` runs
  `GuardianProbe` every five minutes when the site has a domain claim
  (`SiteOptions::probe`, `ProbeOptions`, else `data/claims/{domain}.json`), opening an
  incident on `RunningSite::incidents` on drift; `inbox::SiteInbox` reads the site's email and
  the chat messages of unpaired accounts on the channels the kernel has, answering in their
  own thread (`inbox::ChannelReplier`) and filing a typed signal for a flagged message; and
  `deploy::config::configure` reads `email.json`, `upkeep.json`, `wallet.json` and the domain
  claim into `SiteOptions`, which `adk-website start` and `adk-rust website new` call.
  `adk-server` re-exports `META_THREAD`, `META_BINDING` and `META_LIMIT_KEY`; `adk-site`'s
  `HttpClient::take_connections` drains the connection records a long-running caller keeps.
