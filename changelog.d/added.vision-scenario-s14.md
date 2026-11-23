- **Vision scenario S14, the Screen copilot** (`examples/vision`, `vision-scenarios`): a
  harness in Ask mode with the browser and computer-use packs copies a customer record from a
  web CRM served in process into a desktop form on a display at device pixel ratio 2. Browser
  marks from `browser_elements` are clicked by selector; desktop targets come from PP-OCRv6
  marks per window, coordinates in the screenshot fitted to Claude's limits, `ScreenSpace` and
  target evidence; one `ScreenPlan` is checked again before every step, and every desktop
  action runs inside an approved `computer_use_execute` and is verified on a fresh screenshot.
  The seeded mis-grounding — two identical Save buttons — is caught by the verifier and
  grounded again, a step aimed at a third window is blocked at preview, and the saved record
  matches the CRM's field for field. The WebDriver server and the desktop are stand-ins;
  `docs/official_docs/vision/scenarios.md` says what stands in for what.
