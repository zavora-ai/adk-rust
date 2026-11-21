- **`IsolationStatus` gains `Remote` and is `#[non_exhaustive]`** (`adk-harness`): the
  remote shell reports `IsolationStatus::Remote { provider, off_host }`, serialized with
  `kind: "remote"` and `offHost`. `SandboxError` gains `Unsupported` and `Remote` with the
  `workspace` feature.

  Migration: add a wildcard arm to a `match` on `IsolationStatus` outside `adk-harness`.
