- **"While you were away" shows what agents did, by name** (`adk-surface`): the engine's
  briefing (the terminal's, and the web home's without a kernel ledger) lists the events the
  kernel's `GET /kernel/away` lists — finished, failed, stopped, suggested, asked, decided,
  stopped and resumed all — instead of every ledger row, so the kernel's wakes and process
  state changes no longer flood it. A process row's **Done** and **Failed** come from the
  row's `effect`, where the `/proc` ledger writes the state (they never showed before), and
  agents are named as the roster names them ("ADK-Rust", not
  "Urn:adk:agent:adk rust/adk rust").
