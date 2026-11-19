- **Harness vision pack** (`adk-harness`, feature `vision`): `HarnessAgentBuilder::pack_vision`
  adds the governed vision tools, classed `Read` on the device and `External` off it
  (`CallClasses`), the `.adk/vision.toml` egress rules with faces and personal text always
  redacted, held calls carrying the egress image, and the image privacy guardrail on a remote
  harness model. Recognised text reaches the model inside an untrusted block.
