- **`adk-rust vision run describe` names the in-process describer** (`adk-cli`): an unknown
  `--backend` no longer says an on-device describer over mistral.rs is not available; it names
  `claude`, `gemini` and `nim` and points to `LlmDescriber::mistralrs` (`adk-vision` feature
  `mistralrs`) for programs. The `--model` help names NIM's default as
  `nvidia/nemotron-3.5-super-vl-120b-a12b`.
