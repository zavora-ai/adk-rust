- **MCP OAuth2 tokens refresh on long-lived connections** (`adk-tool`): the HTTP
  transport attached the token fetched at connect time to every request, so a
  connection failed once that token expired. Each request now carries the current
  token, refreshed before the `expires_in` the token response declared, and a request
  answered with HTTP 401 is sent once more with a newly fetched token. A 401 that
  still reaches the toolset triggers a reconnect through its connection factory, and
  the rejected request is retried because it never ran. Concurrent requests share
  one token fetch.
