- **Approve with the image** (`adk-core`): a held call carries the exact image it would send,
  keep or act on. `Tool::approval_evidence` declares it; `GovernedCall`,
  `ToolConfirmationRequest`, `ApprovalRequest` and `ApprovalRoute` gain `evidence:
  Vec<Evidence>`, where `Evidence::Image(ImageEvidence)` holds the image's digest, its
  `ArtifactLocator`, MIME type, dimensions, and an optional regions overlay and caption. The
  evidence digests are part of the call's fingerprint, so a decision binds to the exact image:
  a call whose image changed before it ran is held again. Calls without evidence keep their
  fingerprints. Governed audit records and the approval service's decision records carry
  `evidenceDigests`, and every `ApprovalStore` round-trips evidence (conformance case
  `evidence_round_trips`).
