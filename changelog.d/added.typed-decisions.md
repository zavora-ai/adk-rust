- **Typed decisions** (`adk-core`, `adk-agent`, `adk-graph`, `adk-rust-macros`):
  `adk_core::decide::<T>` has a model pick one value of a `Choice` type, such as an enum of
  routes, and returns a `Decision` with the option, the model's rationale and its confidence.
  The request carries a JSON response schema whose `choice` enumerates the options, declares no
  tool and forces no `tool_choice`; a reply that names no option fails with
  `decision.invalid_choice`. Every call runs in a `ModelCallScope`, so it counts against the run
  budget and spend ledger. `Decider` adds an instruction, an explicit scope and `decide_among`
  for options known at run time. `DecisionAgent` routes a workflow by a typed decision with a
  confidence floor and a fallback route, `adk_graph::DecisionNode` is the same router as a graph
  node, and `#[derive(Choice)]` implements `Choice` for an enum of unit variants.
