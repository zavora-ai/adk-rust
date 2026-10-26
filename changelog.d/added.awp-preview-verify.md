- **`awp verify` against the preview in the launch gate** (`adk-awp`, `adk-site`,
  `adk-website`): `Verifier::verify_preview` runs the AWP 2.0 §15 procedure against the next
  release before it is logged, binding its manifest by the digest its staging recorded.
  `adk-site`'s `gate::PreviewVerification` (feature `verify`) stages a build over the site log
  its host serves and verifies it in the full tier as `GateCheck::AwpVerify`; a served site's
  launch gate runs it, reading the Agent Card from its own edge (spec 3.5 §7.8.1).
