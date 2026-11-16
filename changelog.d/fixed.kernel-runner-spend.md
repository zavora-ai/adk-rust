- **A kernel counts the spend of any hosted `Runner`** (`adk-kernel`, `adk-server`): a
  kernel built from code with `KernelBuilder::agent` attributes each turn's cost, tokens
  and model calls from the usage its model responses report, so `ps` shows the spend and
  `Quotas::daily_spend_cap` and the per-unit caps hold the next process. Before, only a
  harness's `ProgressSource` reported usage and the cap never tripped. The new
  `WorkerHooks::on_event` hands a host each event of an agent turn.
