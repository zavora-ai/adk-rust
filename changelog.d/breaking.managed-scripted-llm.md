- **`adk_managed::ScriptedLlm` is `adk_model::ScriptedLlm`** (`adk-managed`): the managed
  runtime re-exports the scripted model from `adk-model`. `ScriptedLlm::new(name, turns)`
  still accepts a `Vec<ScriptedTurn>`, since a `ScriptedTurn` converts into a
  `ScriptedResponse`. A call past the last scripted turn now fails with
  `model.scripted.exhausted` (`SCRIPT_EXHAUSTED_CODE`) instead of answering
  `[ScriptedLlm: no more scripted turns]`. `turns_consumed()` and `total_turns()` are
  removed: `requests().len()` counts the calls made, and `remaining()` the scripted replies
  not yet used.
