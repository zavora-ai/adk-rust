- **Run egress ceiling and tool egress declarations** (`adk-core`, `adk-runner`, `adk-tool`):
  `Runner::builder().egress_policy(..)` and `RunConfig::egress_policy` set the operator's network
  egress ceiling, which a run's own policy can only narrow. `Tool::egress(&args)` declares a
  call's destinations as `ToolEgress` (`Undeclared` by default, `None`, `Configured`,
  `Enforced`, or `Destinations`); under a run egress policy `authorize_tool_call` refuses an
  undeclared call or one the policy does not cover before the tool policy, guardrails, or
  confirmation, and `ToolContext::egress_policy()` gives tools the policy to enforce on their
  own connections. `FunctionTool::with_egress` and `with_egress_fn` declare function tools,
  `AgentTool` declares `Configured` and carries the policy into its delegate, and every tool
  wrapper in the workspace forwards its inner tool's declaration. Refs #1281.
