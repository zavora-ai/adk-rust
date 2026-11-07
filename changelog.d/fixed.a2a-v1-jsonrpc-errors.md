- **A2A v1 JSON-RPC errors and streaming failures follow the spec** (`adk-server`): a body that
  is not JSON gets error `-32700` with `id: null`. JSON that is not a request object gets
  `-32600`, with the request's `id` when it can be read. Both come back as HTTP 200, where they
  used to be an HTTP 400 or 422 with a plain-text body. JSON-RPC responses use
  `application/json` (§9.1), and the REST binding keeps `application/a2a+json`. A streaming run
  that fails now ends on a failed status that carries the error text, with no error frame after
  it (§3.1.2).
