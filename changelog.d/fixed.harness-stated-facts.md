- **A preference the person states outright reads "You said this"** (`adk-harness`):
  `memory_remember` takes the person's own words as `quote` and stores the fact `Known`,
  source `UserStated`, when the quote is found in one of their messages in the session, read
  from the session store; without a quote, or with one found nowhere in their messages, the
  fact stays a guess and the tool's answer says so (spec 3.3 §8.1, spec 3.1 §9.2 I41).
