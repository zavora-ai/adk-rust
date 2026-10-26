- **AG-UI interrupts offer only the answers a resume can give** (`adk-server`; spec 3.3
  §10 S2, S-I1): `metadata.answerable` and `responseSchema` follow what the caller may answer
  — no answer on a server without a `RequestContextExtractor`, or for an entry the session's
  owner may not decide — and the schema offers only the answers the entry's route allows.
  `metadata.agent` is the acting agent's name, not its identity URN. An answer the entry
  does not allow is refused as "This decision does not offer Deny" (or Allow once, or Edit).
- **AG-UI reports store failures and changed answers** (`adk-server`; spec 3.3 §10 S2): an
  inbox that cannot be read when a run ends gives `RUN_ERROR` with the store's code instead
  of an interrupt marked unanswerable, a failed read of whether a decision waits to run
  refuses the resume as one to try again, and a yes with other `editedArgs` than an entry was
  approved with is reported with `code` and `message`.
- **AG-UI answers are on record with their surface** (`adk-server`; spec 3.3 §11): each
  answer a `resume` gives is written to the audit trail attached to the server's
  `GovernanceControl` as a `surface.decide` record with `surface: "ag_ui"` and how it was
  given, so "What your agents did" says where the answer came from.
