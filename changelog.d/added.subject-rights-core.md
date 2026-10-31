- **Visitor subjects, consent records, and subject rights** (`adk-core`): `SubjectKind::Visitor`
  names a person known only by a random `VisitorId`. `ConsentStore` keeps consent per subject and
  `ConsentPurpose`, `SubjectRegistry` keeps each subject's pseudonym, links, and erasure state, and
  `SubjectRights` exports, erases, and links a subject across every registered `SubjectDataStore`
  adapter with a per-store report. In-memory implementations, three conformance suites, and an
  `erasure` audit record kind ship with it. A visitor's run answers `is_authenticated()` with
  `false`, and `ApprovalService::decide` refuses a visitor with `approval.visitor_not_allowed`.
