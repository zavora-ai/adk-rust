- **Fetch tools refuse URLs the model composed** (`adk-core`, `adk-harness`, `adk-browser`):
  `adk_core::egress::ContextUrls` collects the URLs a conversation's user messages and tool
  results hold, and a fetch tool admits only those, so a model that read a prompt-injected
  page cannot carry data out in a query string, path or host it built. The harness
  `web_fetch` and browser-pack navigation apply it in Auto mode by default
  (`WebConfig::url_provenance`: `Off`, `Unattended`, `Always`), refuse a URL outside it as
  `harness.web.url_not_in_context` before resolving the host, and audit the refusal as
  `harness.web.fetched` `notInContext` by digest; `WebConfig::url_sources` picks the sources,
  the shell's output excluded by default. `WebConfig::fetch_domains` is a fetch filter
  (allowed or blocked domains, subdomains included) the policy enforces before anyone is asked
  and the fetcher re-checks at every redirect hop (`harness.web.domain_not_allowed`).
  `web_fetch`'s `as: "markdown"` writes relative links as absolute URLs.
  `BrowserToolset::with_url_provenance` applies the rule to `browser_navigate`,
  `browser_new_tab` and `browser_new_window` outside the harness.
