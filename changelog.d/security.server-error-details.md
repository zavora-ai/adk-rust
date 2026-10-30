- **Run and governance error responses honour `expose_error_details`** (`adk-server`): REST
  Problem JSON, the SSE error event and AG-UI `RUN_ERROR` of the run endpoints, and the `503`
  of `POST /api/admin/freeze` and `/api/admin/unfreeze` carried the error's own message, so a
  model provider's response, a store's driver text, or an internal host reached the client
  whatever the setting. With `SecurityConfig::expose_error_details` off — the default — a
  message reaches the client only when the error rejects the request and the server wrote it
  (category `invalid_input`, `unauthorized`, `forbidden`, `not_found`, `unsupported`,
  `conflict`, or `resource_exhausted`; component `server`, `auth`, `session`, `agent`, or
  `guardrail`; no upstream status code or provider); every other message is replaced by a
  generic one for its category, and the full error is logged inside the request's span with
  its `x-request-id`. `code`, `category`, `requestId`, and `retryAfter` are kept, and the AG-UI
  `RUN_ERROR` now carries `code`. `GovernanceController::with_error_details` sets the flag on
  a controller mounted by hand.
