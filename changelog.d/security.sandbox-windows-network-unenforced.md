- **The Windows sandbox says it leaves the network open** (`adk-sandbox`): the
  `SandboxPolicy` rustdoc stated that denying the network blocks all network access on every
  platform, while the Windows restricted-token enforcer has no network filter and never
  applied it. The rustdoc now states which enforcer applies each network mode, and the
  enforcer refuses a restrictive one unless the caller acknowledges the open network (see the
  `NetworkAccess` entry under Breaking).
