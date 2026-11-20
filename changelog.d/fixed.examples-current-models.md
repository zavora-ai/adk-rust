- **Two examples run on current models** (`examples/anthropic_server_tools`,
  `examples/desktop_audio`): the Anthropic native-tool matrix defaults to the catalog's
  `claude-sonnet-5-5` in place of the retired `claude-sonnet-4-20250514`, which the API answers
  with `not_found_error`. The desktop voice agent runs its `LlmAgent` on `gemini-3.8-flash` in
  place of the deprecated `gemini-2.5-flash`, and its banner prints the models the STT and TTS
  providers bill rather than fixed names.
