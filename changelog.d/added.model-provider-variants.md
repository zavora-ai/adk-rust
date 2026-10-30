- **`ModelProvider` names every provider with its own client** (`adk-model`, `adk-cli`):
  `Muse`, `Openrouter`, `Opencode` (Zen), `OpencodeGo`, `Bedrock` and `AzureAi` join the six
  earlier variants, and `ModelProvider::feature` returns the Cargo feature that compiles each
  one; a test holds the variants equal to `all-providers`. OpenAI-compatible presets stay
  `OpenAICompatibleConfig` presets. `catalog::OPENCODE_DEFAULT` (`deepseek-v4.1-flash`) is the
  default model of both OpenCode services. `adk-rust --provider` takes `muse`, `openrouter`,
  `opencode`, `opencode-go`, `bedrock` and `azure-ai` behind the new adk-cli features of the
  same names (in `all-providers`); a build without the feature names it to reinstall with.
