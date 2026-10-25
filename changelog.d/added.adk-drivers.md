- **`adk-drivers`: service drivers for DNS, email, analytics, social and advertising**
  (`adk-drivers`, new, Experimental; spec 3.5 §7.13): each class is a contract for
  in-process adapters, the kernel `DriverManifest` of its tools with their effect classes
  and partitions, a check that any MCP server's manifest and tool listing satisfy it, and a
  conformance suite. `DriverCapPolicy` holds DNS changes and campaign launches in every mode
  and within-budget calls beyond the owner's budget; the email and social outboxes end every
  message with the AI disclosure line, honour unsubscribes and revocations before the next
  send, check consent at send time, refuse bulk email until SPF, DKIM and DMARC pass, and
  keep `reply_in_thread` to the thread's sender within a playbook template. Reference
  adapters: RFC 2136 dynamic DNS with TSIG (checked against BIND's `nsupdate` and `dig`),
  RDAP expiry, IMAP and SMTP, the Plausible Stats API, a Search Analytics API, the Mastodon
  client API, and recorded advertising responses.
