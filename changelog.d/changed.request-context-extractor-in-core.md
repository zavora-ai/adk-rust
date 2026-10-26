- **`RequestContextExtractor` lives in `adk-core`** (`adk-core`, `adk-server`, `adk-auth`): the
  trait and `RequestContextError` move to `adk_core::request_context` behind the new
  `auth-bridge` feature; `adk_server::auth_bridge` re-exports both under the same names.
  `adk-auth`'s `auth-bridge` feature no longer depends on `adk-server`, so `adk-server` can depend
  on `adk-auth`; its `adk-server` feature remains as a no-op.
