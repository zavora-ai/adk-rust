- **`adk-rust` keeps its action and spend ledgers** (`adk-cli`, spec 3.1 §7.12, §7.13; spec
  3.3 §12 phase 1): the REPL, `code`, `goal`, `serve` and `adk-rust up` record in
  `SqliteActionLedger` and `SqliteSpendLedger` in `harness.db`, and `serve --store` in the
  shared PostgreSQL or Redis ledgers, so a call with side effects that began before a restart
  is answered from its record rather than run again. A sealed `harness.db` keeps no error
  text of a failed call.
- **One decision per call through `Harness::decide`** (`adk-harness`, spec 3.3 §7.3, §11
  S-I2): of two decisions for one call recorded at once in one process, the second is refused
  with `harness.decision.invalid` instead of both being written.
- **A decided CodeAct call is answered** (`adk-agent`, spec 3.3 §7.3): the event that records
  a resumed held call's result carries its function response, so the session no longer holds
  the call and compaction passes it.
- **A stranger the chat cannot reply to keeps one request** (`adk-kernel`, spec 3.3 §10 K1,
  §11 S-I6): a pairing code that cannot be sent leaves the request waiting, marked
  undelivered (`ChannelPairingRequest::undelivered`), with its place and its one record; the
  account's next message is sent a new code in its place.
- **Take over ends in one place** (`adk-harness`, spec 3.3 §8.2, §10 H2): the waiting call
  decides its entry when it takes the typed secret, so a secret that arrives after Stop all,
  the timeout or a cancel reaches no call, `TakeOver::provide` answers
  `harness.take_over.not_waiting`, and the entry ends cancelled rather than approved.
- **The personal preset shows its limits everywhere** (`adk-cli`, spec 3.3 §10 H1, §13 Q7):
  `code`, `goal`, `ultracode` and `--voice` print quiet time and the daily spending limit,
  and `adk-rust up --home` says when the other commands read another `personal.toml`.
