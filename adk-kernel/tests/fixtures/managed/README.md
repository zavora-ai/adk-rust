# Kernel definition fixtures

Golden fixtures for the kernel block of a managed agent definition (spec 3.2 §7.13, §10 M1),
read by `tests/managed_def_fixtures.rs`. Each file holds:

| Key | Meaning |
|---|---|
| `name`, `description` | What the fixture shows |
| `agent_def` | The `ManagedAgentDef`, without the kernel block |
| `kernel` | The block, written as an object in the kernel's camelCase wire shape; the test stores it as JSON under `adk.kernel` |
| `expect.canonical` | The block as `KernelDef` serializes it, defaults explicit: the driver manifest's server settings, the proactivity policy's and the consolidation tick's |
| `expect.consolidation` | Whether the block turns the consolidation tick on |
| `expect.triggers` | The trigger names a tenant kernel registers for the definition |
| `expect.metadata_kept` | Metadata keys beside the block that stay as written |
| `expect.error_contains` | For a refused block: text every refusal message carries |

The test round-trips each definition through JSON, as the managed state store does, before
it reads the block.
