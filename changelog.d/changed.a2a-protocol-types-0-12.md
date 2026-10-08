- **`a2a-protocol-types` upgraded from 0.5 to 0.12.1** (`adk-server`, feature `a2a-v1`): the
  pin was five minor versions behind. Two changes in the dependency's 0.7.0 release reach this
  crate's `a2a-v1` surface. `JsonRpcRequest.id` is now the three-state `JsonRpcRequestId`
  (`Absent`/`Null`/`Value`), so an explicit `"id": null` is treated as the call JSON-RPC 2.0
  says it is rather than collapsing into a notification; responses are unchanged, because
  `to_response_id()` maps both `Absent` and `Null` to the `None` the previous `Option<Value>`
  produced. `TaskPushNotificationConfig::task_id` and `AuthenticationInfo.credentials` are now
  `Option<String>`, matching the canonical schema that previously rejected valid cross-SDK
  payloads at parse time; a standalone push-config create with no task id is now refused with
  a structured invalid-params error, and a push notification whose config carries no
  credentials now omits the `Authorization` header rather than sending an empty or placeholder
  bearer token.
