# Contributors

ADK-Rust is built by the people below. Thank you.

## v2.0.0

Many thanks to [@joseph-wortmann](https://github.com/joseph-wortmann) — the Code Agents proposal (#380) and the `CodeActAgent` with its Monty-backed Python runtime (#399, #525). [@mikefaille](https://github.com/mikefaille) — realtime audio and LiveKit: typed audio preserved through the bridge (#431, #432), workspace-wide LiveKit/webrtc alignment (#433), the 0.8.1 upgrade (#543), disconnect reasons on a closed stream (#540), key-order-independent `SchemaCache` keys (#531), a unified workspace `jsonschema` (#533). [@caibirdme](https://github.com/caibirdme) — split the `LlmAgent` run flow into a reusable `ToolExecutor` (#522). [@NikiKrutan](https://github.com/NikiKrutan) — `reasoning` fallback for OpenAI-compatible providers (#388), duplicate `provider_metadata` key in `Event` serialization (#415). [@mscharley](https://github.com/mscharley) — Anthropic `web_fetch_20250910` tool type and result blocks (#381). [@hiratara](https://github.com/hiratara) — `functionDeclarations` field names for Gemini 3 compatibility (#389). [@chathaway-codes](https://github.com/chathaway-codes) — configurable parallel tool calls for OpenAI (#387). [@nullsauce](https://github.com/nullsauce) — parallel tool-call indices in the OpenRouter adapter (#410). [@morlay](https://github.com/morlay) — streaming content accumulation in the DeepSeek final event (#398). And to everyone who filed an issue or a reproduction this cycle. [Get started →](https://github.com/zavora-ai/adk-rust/wiki/quickstart)

## v1.0.0

[@mikefaille](https://github.com/mikefaille) — AdkIdentity design, realtime audio, LiveKit bridge, skill system. [@rohan-panickar](https://github.com/rohan-panickar) — OpenAI-compatible providers, xAI, multimodal content. [@dhruv-pant](https://github.com/dhruv-pant) — Gemini service account auth. [@tomtom215](https://github.com/tomtom215) — A2A Protocol v1.0.0 types crate ([a2a-protocol-types](https://crates.io/crates/a2a-protocol-types)), Foundation-verified wire types powering our A2A v1 layer. [@danielsan](https://github.com/danielsan) — Google deps issue & PR (#181, #203), RAG crash report (#205). [@CodingFlow](https://github.com/CodingFlow) — Gemini 3 thinking level, global endpoint, citationSources (#177, #178, #179). [@ctylx](https://github.com/ctylx) — skill discovery fix (#204). [@poborin](https://github.com/poborin) — project config proposal (#176). [@chillin-capybara](https://github.com/chillin-capybara) — ACP integration, adk-acp crate. [@baotao2006](https://github.com/baotao2006) — UTF-8 boundary audit, CJK search/skill/eval fixes (#349, #357).

Everyone who filed an issue, a reproduction, or a review comment shaped these
releases too. If your work is missing here, open a pull request — the omission is
an oversight, not a judgement.
