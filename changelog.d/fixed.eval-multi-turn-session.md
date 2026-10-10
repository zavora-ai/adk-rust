- **Multi-turn eval cases carry their conversation** (`adk-eval`): `Evaluator` runs the
  turns of a case through one session of an in-memory session service with a `Runner`,
  so each turn sees the history and state of the turns before it, and applies the case's
  `session_input` (`app_name`, `user_id`, initial `state`). Every turn previously ran
  with an empty history and state, and `session_input` was ignored.
  `Evaluator::evaluate_multi_turn` shares one session across its turns the same way.
