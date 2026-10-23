- **A sleeping process stays asleep through a freeze** (`adk-kernel`, spec 3.2 K-I2): a
  freeze landing inside a sleeper's first claim handed the run back with its checkpoint
  recorded, and once its sleep request was cleared — by `suspend`, or by the 250 ms
  mirror — the claim after `unfreeze` ran it and it called the model. A claim now parks
  every process whose wake set is still recorded, and a wake clears the set before the
  run becomes claimable. A queued sleeper `resume`d before its first claim now runs
  instead of parking as suspended.
