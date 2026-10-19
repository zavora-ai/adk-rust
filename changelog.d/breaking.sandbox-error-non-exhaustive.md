- **`SandboxError` is `#[non_exhaustive]`** (`adk-sandbox`, `adk-code`): its `workspace`
  variants exist only with that feature, so an exhaustive match outside the crate broke as
  soon as any crate in the build enabled it; a match now needs a wildcard arm. `adk-code`
  failed to build when `adk-sandbox`'s `workspace` feature was enabled alongside it, and now
  maps those variants to an error result.
