- **Unknown paths answer `404` on an A2A server** (`adk-server`): with `with_a2a`, a path no
  route serves (`/`, `/ui/`, a typo) answered the A2A v1 binding's `VERSION_NOT_SUPPORTED`
  (HTTP `200` with `-32009`, or `400`), and `401` once a `RequestContextExtractor` was set.
  `A2aV1Server::rpc_router` and the A2A authentication now cover only the A2A routes, so every
  other path keeps the app's `404`.
