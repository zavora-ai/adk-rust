- **Perceiver, reasoner and verifier teams** (`adk-vision`, `adk-agent`, `adk-harness`): feature
  `team` adds `adk_vision::team` — a `Perceiver` that refuses at construction a remote model, a
  runtime with a remote backend or any tool that declares network use; a `Reasoner` that sees
  only crops the egress gate released; a `Verifier` that re-checks a claim against the pixels
  with local perception, by default on another provider; and `VisionTeam`, which holds a
  reasoner/verifier disagreement for a person as one governed call with both opinions and the
  frame as evidence. `VerifierLoop` compares a fresh observation after an action, re-grounds
  once and then holds. `TeamArchitectureTemplate::PerceiveReasonVerify` lowers the roles to a
  `TeamSpec` with their capabilities and edge schemas, and the harness adds
  `MemberSpec::perceiver`, `reasoner` and `verifier` with the vision pack's privacy guardrail on
  every remote member model.
