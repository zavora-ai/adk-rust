- **`Llm` gains `data_locality`** (`adk-core`): a model wrapper forwards it to the model it
  wraps, or the wrapped model reports the default, `DataLocality::Remote`.

  Migration: add `fn data_locality(&self) -> DataLocality { self.inner.data_locality() }` to
  an `Llm` wrapper; a wrapper over several models returns `DataLocality::least_local` of them.
