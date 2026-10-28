# muse-agentic

Meta Muse Spark agent with tool calling: `muse-spark-1.3` behind `LlmAgent`
answers a weather question by calling a `get_weather` tool through `Runner`,
then summarizes the result.

## Run

```bash
META_API_KEY=... cargo run --manifest-path examples/muse_agentic/Cargo.toml
```

`MODEL_API_KEY` also works. The run exits non-zero when the tool is never
called or the final answer is empty.
