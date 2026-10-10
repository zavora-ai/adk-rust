- **Browser navigation refuses private network addresses** (`adk-browser`):
  `browser_navigate`, `browser_new_tab`, and `browser_new_window` checked only the URL
  scheme, so a model could open `http://169.254.169.254/` or a service on `localhost`.
  They now refuse loopback, private, link-local, shared-address, and cloud metadata
  addresses, including host names that resolve to them, until
  `with_private_network_access(true)` permits them on the toolset or the tool.
- **`browser_file_upload` is opt-in and confined** (`adk-browser`): the `Full` profile and
  `BrowserToolset::new` offered the tool for any model-chosen path. No profile includes it
  now; `BrowserToolset::with_file_upload(roots)` adds it, and `FileUploadTool` uploads
  only existing files inside its roots after resolving symlinks, refusing every upload
  when it has none.
