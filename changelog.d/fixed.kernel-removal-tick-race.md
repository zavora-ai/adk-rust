- **A removed agent's rules are counted, and its removal recorded, once** (`adk-kernel`, spec
  3.3 S-I16): the tick that finishes interrupted removals could start on a removal a person
  was still finishing, revoke the agent's standing rules first and write a second
  `kernel.agent.removed`, so the person's outcome reported `rules: 0`. A replica now
  finishes one removal at a time, and the tick re-reads a record before finishing it.
