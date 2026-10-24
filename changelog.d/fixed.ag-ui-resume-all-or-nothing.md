- **AG-UI resumes are all or nothing** (`adk-server`): every resume entry is checked against
  its inbox entry before any is decided, and the answers are recorded only after the rest of
  the request is valid, the session and agent are loaded and the server is not frozen, so a
  refused or failed resume decides nothing. An entry already decided whose call has not run
  lets the run start instead of refusing the resume; one interrupt answered twice is `400`.
  `adk.decision` carries the entry's `version`, and an answer not recorded carries `code` and
  `message`. A call with no inbox entry ends the run with `metadata.answerable: false` and no
  `responseSchema`; `metadata.agent` is the acting agent the inbox entry records; and `GET
  /api/ui/capabilities` lists `interrupt_resume` only on a server that can decide resumes
  (spec 3.3 §10 S2).
