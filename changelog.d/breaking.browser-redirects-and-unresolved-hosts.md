- **Browser navigation checks redirects and refuses unresolvable hosts** (`adk-browser`):
  the private network check covered only the URL the model asked for, so a public page
  that redirected to `http://169.254.169.254/` was loaded and returned to the model, and
  a host name that did not resolve on the agent host was let through for the browser to
  resolve. `browser_navigate`, `browser_new_tab`, `browser_new_window`, `browser_back`,
  `browser_forward`, and `browser_refresh` now check the page the browser ends on; a
  refused page fails the call and is replaced with `about:blank`. A host name that does
  not resolve on the agent host is refused until `with_unresolved_hosts(true)` permits
  it on the toolset or the tool, for example when the browser resolves names through a
  proxy. Navigation started by the page or by an interaction tool is not checked.
