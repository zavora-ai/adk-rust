- **AG-UI never approves a high-risk call or one that spends money** (`adk-server`; spec 3.3
  §7.2, §10 S2): an AG-UI front end cannot confirm it is the person, so an interrupt whose
  inbox entry is high or critical risk, or a `payments_*` call, offers only a denial, and a
  `resume` that approves or edits one is refused with `surface.answer_not_allowed`. It is
  approved where the person can confirm it is them.
- **Two AG-UI resumes of a thread never run at once** (`adk-server`; spec 3.3 S-I2): a
  `resume` sent while an earlier one's run is still going is refused with
  `approval.resume_in_progress`, so a double click or a retry no longer starts a second run
  that holds an already approved call again and asks for a new decision.
- **An AG-UI resume never approves arguments the person was not shown** (`adk-server`; spec
  3.3 S-I2): without an echoed `metadata.version`, an entry another approver edited since the
  interrupt is a version conflict; the `adk.decision` update and the interrupt's
  `metadata.arguments` carry the edited arguments.
