- **`ToolUnionParam` gains two variants** (`adk-anthropic`): `WebSearch20260209` and
  `WebFetch20260209`. `ToolUnionParam` is not `#[non_exhaustive]`, so an exhaustive `match`
  on it needs two more arms.
