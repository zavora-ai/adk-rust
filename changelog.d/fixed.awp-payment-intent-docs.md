- **AWP 1.x payment documentation names what exists** (`awp-types`, spec 3.5 §10 X2): the
  `PaymentIntent` docs no longer describe an `adk-awp` bridge to `adk-payments`, which never
  existed; they name `adk-gateway` as the separate repository that uses the type and state that
  no crate here computes or verifies its `signature`. `PaymentPolicy::evaluate` drops a branch
  that returned what the default did; its decisions are unchanged.
