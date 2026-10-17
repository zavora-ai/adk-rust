- **`adk_managed::ScriptedLlm` is `adk_model::ScriptedLlm`** (`adk-managed`): the managed
  runtime re-exports the scripted model from `adk-model`. `ScriptedLlm::new(name, turns)`
  still takes `Vec<ScriptedTurn>`, which converts into `ScriptedResponse`. A call past the
  last scripted turn now fails with `model.scripted.exhausted` instead of answering
  `[ScriptedLlm: no more scripted turns]`, and `turns_consumed()` and `total_turns()` are
  replaced by `requests()` and `remaining()`.
