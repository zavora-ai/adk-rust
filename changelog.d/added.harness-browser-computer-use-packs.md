- **Browser and computer-use packs** (`adk-harness`, spec 3.1 §7.24): `HarnessAgentBuilder::pack_browser`
  puts `adk-browser`'s tools under the harness policy — observation as `Read`, navigation as `Web`
  decided by the URL's host under `WebConfig::allowed_domains`, actions held for approval in Ask and
  Auto and denied in ReadOnly and Plan — with the §7.20 URL checks before the browser moves, typed
  secrets refused, and every result screened and delivered as untrusted text; `browser_evaluate_js`
  and `browser_file_upload` are never attached. `pack_computer_use` adds `computer_use_observe`,
  `computer_use_preview` and `computer_use_execute`; the last is class `ToolKind::ComputerUse`, a
  never-waivable one-shot approval bound to the runtime's action and policy digests that answers
  `adk-computer-use`'s approval interrupt, so a desktop action runs once through the reference graph
  or not at all. `Pack::Browser` and `adk-rust --pack browser` (adk-cli feature `browser`, the
  WebDriver at `ADK_WEBDRIVER_URL`) install the browser pack; `--pack computer-use` names the library
  API. New audit records: `harness.browser.navigated`, `harness.browser.acted`,
  `harness.computer_use.previewed`, `harness.computer_use.executed`.
