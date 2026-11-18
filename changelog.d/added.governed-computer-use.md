- **Governed computer use and browser use** (`adk-core`, `adk-agent`, `adk-computer-use`,
  `adk-browser`): `ComputerUseToolset` runs each member of Claude's computer use toolset as a
  governed tool (`computer.left_click`, `computer.screenshot`, ...) over an
  `adk_core::ComputerActuator`, and `BrowserUseToolset` runs the browser use members over a
  WebDriver session. Every action passes through `authorize_tool_call`, so tool policy,
  confirmations, standing approvals, the kill switch, run budgets and the action ledger apply,
  and screenshots return as images. `WebDriverComputer` makes a browser viewport a computer use
  display. `Tool::batch_group` declares an ordered batch: `LlmAgent` runs its calls in order and
  answers every call after the first that is refused, fails or is held with the batch's halt
  text instead of running it. No desktop actuator ships; an application supplies one.
