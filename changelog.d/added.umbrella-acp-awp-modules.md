- **`adk_rust::acp` and `adk_rust::awp`** (`adk-rust`): the `acp` and `awp` features compiled
  `adk-acp` and `adk-awp` but re-exported neither, so the feature gave the umbrella crate no
  way to reach them. Both are now modules of the umbrella crate.
