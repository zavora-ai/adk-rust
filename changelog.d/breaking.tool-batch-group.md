- **`Tool::batch_group` and the computer use types** (`adk-core`): `Tool` gains
  `batch_group()`, defaulting to `None`; a wrapper that implements `Tool` forwards it, or the
  wrapped tool's calls no longer stop at the first failure in a batch. The new
  `ComputerAction`, `ComputerActionKind` and `ActuationOutput` enums are `#[non_exhaustive]`.
