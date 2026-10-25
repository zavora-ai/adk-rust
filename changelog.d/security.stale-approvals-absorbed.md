- **An approval left by another decision no longer authorizes a later identical call**
  (`adk-core`, `adk-agent`, `adk-harness`; spec 3.3 §7.3): an inbox outcome applied while the
  confirmation handler was asked, or before a call-ID decision or fingerprint approval decided
  the call, is absorbed when the call is decided, as an outcome still being written already
  was. `CodeActAgent` on a runtime that cannot pause withdraws the call it refuses into the
  script, and `Harness::decide` refuses a decision for a call that an approver, an expiry or
  another decision settled while the decision was being recorded, instead of writing an
  approval nothing consumes.
