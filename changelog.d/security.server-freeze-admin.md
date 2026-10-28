- **The kill switch's freeze and unfreeze need an administrator** (`adk-server`, `adk-cli`):
  `POST /api/admin/freeze` and `POST /api/admin/unfreeze` admit only a caller whose scopes
  include `SecurityConfig::admin_scope` (new field, default `ADMIN_SCOPE`, `"admin"`). An
  authenticated caller without it is refused with `403 governance.forbidden`, and a request
  with no caller, on a server without a `RequestContextExtractor`, with
  `401 governance.unauthenticated`; the control and any `FreezeDelegate` are not reached.
  Before, any authenticated caller, or anyone at all without an extractor, could freeze the
  whole deployment or lift an operator's freeze. `GET /api/admin/governance` is unchanged.
  `GovernanceController::with_admin_scope` sets the scope on a controller mounted by hand.
  The operator that `LoopbackOperatorExtractor` and `BearerTokenExtractor` admit carries the
  `admin` scope beside `operator`, so `adk-rust up` keeps its freeze and unfreeze.
