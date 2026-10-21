- **Governed web access for the harness** (`adk-harness`, feature `web`; spec 3.1 §7.20):
  `web_fetch` and `web_search` are tools of class `Web { allowlisted }`, decided by the
  URL's host against `WebConfig::allowed_domains` (Allow in every mode when allowlisted,
  held in Ask and Auto otherwise). One plain `GET` through the harness's own client with the
  target resolved and classified before connecting and at every redirect; loopback,
  link-local, private and mapped addresses refused; HTML converted to text; results
  delivered inside a delimited untrusted block with injection lines labelled and secret
  patterns redacted; composed URLs capped and refused on a secret pattern. Provider-native
  search (`ProviderWebTools`) stays off until the operator enables it per provider, with the
  allowlist forwarded where the provider supports one. Audit records `harness.web.fetched`,
  `harness.web.searched`, `harness.web.screened`, `harness.web.provider_enabled`. The
  umbrella forwards it as `harness-web`; `adk-rust` declares `web_fetch` by default with
  `--no-web`, `--allow-domain HOST` and `/web [allow HOST]`.
