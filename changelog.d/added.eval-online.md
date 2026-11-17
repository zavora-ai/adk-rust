- **Online evaluation of sampled production runs** (`adk-eval`): `online::OnlineEvaluator`
  judges a seeded fraction of finished runs after they end, off the request path. Its runner
  plugin samples by rate (a fixed hash of seed and invocation id), agent, request metadata and
  outcome (`Completed`, `Errored`, `Incomplete`, `Cancelled`), buffers the sampled run's events,
  and queues the finished run without waiting; `submit` takes runs collected elsewhere. A
  background worker scores each run with rubric, safety and hallucination judges that see the
  run's tool results, `TraceAnalyzer` trajectory efficiency, and thread-level conversation
  metrics over the whole session, under the evaluator's own budget (1,000 judge calls by
  default), an optional spend ledger (agent `online-eval`) and the evaluator's and the run's kill
  switches. Verdicts go to an `OnlineEvalStore` (`InMemoryOnlineEvalStore`,
  `JsonlOnlineEvalStore`), with `OnlineEvalSummary` pass rates, and to `gen_ai.evaluation.result`
  telemetry events. Off unless installed. See
  `docs/official_docs/evaluation/online-evaluation.md`.
