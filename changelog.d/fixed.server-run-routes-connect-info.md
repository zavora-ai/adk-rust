- **The run routes' extractor sees the whole request** (`adk-server`): `POST /api/run`,
  `/api/run/{app}/{user}/{session}`, and `/api/run_sse` hand the `RequestContextExtractor` the
  request's own parts, extensions included, as every other route does; before, they rebuilt a
  header-only request, so an extractor reading the peer address (`ConnectInfo`) refused every
  run. The public handler functions keep their signatures.
