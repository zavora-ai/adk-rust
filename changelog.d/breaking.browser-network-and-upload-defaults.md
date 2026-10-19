- **Browser navigation refuses private network addresses** (`adk-browser`):
  `browser_navigate`, `browser_new_tab`, and `browser_new_window` checked only the URL
  scheme, so a model could open `http://169.254.169.254/` or a service on `localhost`.
  They now refuse loopback, private, link-local, shared-address (`100.64.0.0/10`), and
  cloud metadata addresses, host names that resolve to any of them, and host names that do
  not resolve on the agent host. The page the browser ends on after these tools and
  `browser_back`, `browser_forward`, and `browser_refresh` is checked as well, so a redirect
  to a refused address fails the call and the page is replaced with `about:blank`.
  Navigation started by the page or by an interaction tool is not checked.
  `with_private_network_access(true)` on the toolset or the tool permits private addresses;
  `with_unresolved_hosts(true)` permits host names the agent host cannot resolve, for
  example when the browser resolves names through a proxy.
- **`browser_file_upload` is opt-in and confined** (`adk-browser`): the `Full` profile and
  `BrowserToolset::new` offered the tool for any model-chosen path. No profile includes it
  now; `BrowserToolset::with_file_upload(roots)` adds it, and `FileUploadTool` uploads
  only existing files inside its roots after resolving symlinks, refusing every upload
  until `with_allowed_roots` gives it one.
