# Ideate with Meta

Inventor co-ideation studio on Muse Spark. One pipeline run: intake
normalizes the brief (with an optional sketch the model sees natively and an
optional voice memo transcribed by `muse-voice-transcribe-1.0`), a
three-voice panel ideates in parallel, and a high-effort converger ranks
structured IdeaCards after costing them with a tool. The top card is then
rendered with `muse-image-1.0`.

Agent graph: `SequentialAgent` of intake (`Effort::Low`) →
`ParallelAgent` (wild inventor, engineer, market scout; `Effort::Medium`) →
converger (`Effort::High`, `IdeaCard[]` output schema, `estimate_cost` tool).

## Run

```bash
META_API_KEY=... cargo run --manifest-path examples/ideate_with_meta/Cargo.toml -- --brief "solar backpack"
META_API_KEY=... cargo run --manifest-path examples/ideate_with_meta/Cargo.toml -- --brief "solar backpack" --sketch sketch.png --audio memo.wav
cargo run --manifest-path examples/ideate_with_meta/Cargo.toml -- --dry-run --brief "solar backpack"
```

`MODEL_API_KEY` also works. Live runs assert at least 3 schema-valid cards, at
least one cost call, and a rendered concept image under `out/`; `--dry-run`
drives the same graph with a scripted model (no key, no spend, no render).
