- **The personal preset** (`adk-harness`, `adk-cli`; spec 3.3 §10 H1, §13 Q7):
  `Preset::Personal`, `HarnessAgentBuilder::preset` and `adk_harness::personal::PersonalAgent`
  build a harness whose workspace is `<home>/workspace` — a folder of notes and files, not a
  code repository — with no host shell and long-term memory on. `adk-rust --preset personal`
  and `adk-rust up --preset personal` ask for quiet time and a daily spending limit at the
  first run, keep the answers owner-only in `<home>/personal.toml`, give them to the kernel as
  its quiet hours and daily spend cap, and show both on every start. `Preset::Coding` is the
  `CodingAgent` lowering.
