- **The personal preset** (`adk-harness`, `adk-cli`; spec 3.3 §10 H1, §13 Q7):
  `adk_harness::personal::PersonalAgent` builds a harness whose workspace is
  `<home>/workspace` — a folder of notes and files, not a code repository, kept owner-only —
  with long-term memory on. `Preset::Personal` (`HarnessAgentBuilder::preset`) sets its
  prompt, mode and shell: no host shell, and `bash` only in the OS sandbox or a container,
  with no unsandboxed retry on the host. A later preset replaces an earlier one.
  `adk-rust --preset personal` and `adk-rust up --preset personal` work in that folder
  whatever the current directory, ask for quiet time and a daily spending limit at the first
  run, keep the answers owner-only in `<home>/personal.toml`, and show both on every start.
  `adk-rust up` gives them to the kernel as its quiet hours and daily spend cap; the user
  config's `[kernel]` table wins over them, and the workspace's `.adk/harness.toml` may only
  lower the limit. Every command uses the same home (`[kernel] home`, else the config
  directory's `adk-rust`); an error in `[kernel]` stops only `adk-rust up`, and the others
  note it. `HarnessInfo::shell` reports `None` when a harness offers no shell.
  `Preset::Coding` is the `CodingAgent` lowering.
