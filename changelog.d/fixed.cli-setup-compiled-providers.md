- **First-run setup offers only compiled-in providers** (`adk-cli`): the provider menu lists
  the providers this build includes (`compiled_providers()`), and a saved or `--provider`
  choice that is not compiled in fails with the reinstall hint before any key prompt. Setup
  never prompts without a terminal; it fails with the flags and environment variables to set
  instead. `code`, `goal` and `ultracode` now resolve their model like `chat` and `serve`
  (flags, environment, secure storage, saved config, first-run setup), so without
  `--provider` they use the saved provider rather than always Gemini, and `--instruction`
  adds project guidance to their coding prompt. The default features are now `gemini`,
  `openai`, `anthropic`, `audit` and `persist`.
