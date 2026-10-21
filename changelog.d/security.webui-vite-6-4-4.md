- The server web UI builds with vite 6.4.4 (`adk-server/webui`), closing Dependabot alert #590
  (`launch-editor`'s `escapeCmdArgs()` omitted `%` escaping on Windows in vite <= 6.4.3). The bump
  stays within the 6.x major; the built assets under `adk-server/assets/webui` are unchanged. The
  same lockfile moves `vitest` to 4.1.12 so its `@vitest/mocker` dependency leaves the range
  Dependabot alerts #595 and #596 cover.
