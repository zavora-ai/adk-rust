- **Partitions and crossings** (`adk-kernel`, `adk-harness`, `adk-core`; spec 3.2 §7.8,
  §13 Q3, K-I15): areas of a user's life or teams kept apart, each run by a harness of its
  own, with work across them only through the coordinator and under approval.
  - `Partition` (class, tenant, mode, memory scope, drivers, `CrossPartition`, blocked
    crossings) in a `PartitionStore` on in-memory, SQLite, PostgreSQL and Redis, with
    `partition_store_conformance_tests!`; `Partitions` writes `kernel.partition.created` and
    `kernel.partition.changed` and keeps the crossing ledger.
  - `HarnessAgentBuilder::partition` builds a partition's harness: its mode, its class and
    tenant on every agent's principal through the runner's identity source, every long-term
    fact and its working notes in the partition's memory scope (no user-wide fact is read or
    written), its drivers (`partition_drivers`), and `partition_memory_search` and
    `partition_memory_remember` to reach another partition's facts.
  - `PartitionPolicy` denies a request reaching another partition unless the coordinator
    (`partition_coordinator`) makes it, then requires an approval no standing rule, expiring
    rule or run-wide approval gives, and audits it as `kernel.partition.crossing`. The unit of
    approval is the crossing fingerprint within one run: an approved read lets the
    coordinator's identical reads through for the rest of the run, writes are asked each time,
    and the next run asks again. `CrossingTool` offers the coordinator another partition's
    driver or session tool.
  - The prompt names the crossing and offers "allow this read of *X* for this run", "deny" and
    "deny and block crossings to *X* from this partition" (`ApprovalPrompter::prompt_crossing`,
    `CrossingPrompt`, `CrossingAnswer`).
  - `KernelBuilder::partition_agent` and `Harness::host_partition` host each partition's
    harness as targets bound to the partition; a process of another partition cannot run them
    or continue their sessions. `GET`/`POST /api/kernel/partitions` and
    `POST /api/kernel/partitions/{id}/mode`, with `ControlClient::{partitions, put_partition,
    set_partition_mode}`.
  - Approved reads are kept per run (`CrossingRun`) apart from the append-only ledger: a
    durable run's in the `PartitionStore`, released when the kernel's worker finishes it; an
    invocation's in memory, released when it ends. A driver tool reused as a read must also
    declare itself read-only, so `Financial` and `PublishPublic` calls are never reused.
  - `adk-rust up` with `[[kernel.partitions]]` and `coordinator` in the user config runs one
    harness per partition (`adk-rust@<partition>`), with long-term memory on for partitions;
    `adk-rust partitions [list|add|set-mode P MODE]` lists and changes the table (`adk-cli`).
  - `examples/harness_kernel`: K7 scripted (`k7_partitions`) and live through `adk-rust up`
    (`live_k7_partitions`).
  - `PolicyCall::durable_run_id` (`adk-core`): the governed path tells a policy which durable
    run a call belongs to.
