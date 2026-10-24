- **Partitions and crossings** (`adk-kernel`, `adk-harness`, `adk-core`; spec 3.2 §7.8,
  §13 Q3, K-I15): areas of a user's life or teams kept apart, each run by a harness of its
  own, with work across them only through the coordinator and under approval.
  - `Partition` (class, tenant, mode, memory scope, drivers, quotas, quiet hours,
    `CrossPartition`, blocked crossings) in a `PartitionStore` on in-memory, SQLite, PostgreSQL and Redis, with
    `partition_store_conformance_tests!`; `Partitions` writes `kernel.partition.created` and
    `kernel.partition.changed` and keeps the crossing ledger.
  - `HarnessAgentBuilder::partition` builds a partition's harness: its mode, its class and
    tenant on every agent's principal through the runner's identity source, every long-term
    fact and its working notes in the partition's memory scope (no user-wide fact is read or
    written), the kernel's drivers its own list names and whose manifests serve it
    (`partition_drivers`; any other driver is invisible), and `partition_memory_search` and
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
  - A partition's quotas and quiet hours apply beside the kernel's at admission, at spawn, to
    follow-ups and to the consolidation tick: each quota the lower one, every window held.
  - A partition's harness marks the sessions it runs (`PARTITION_STATE_KEY`) and refuses one
    another partition's harness ran (`harness.partition_session`); its consolidation tick
    works on the partition's facts only; its `propose_followup` items are on the partition's
    agenda and run on its harness.
  - The daily review (`ReviewReport::crossings`, `crossing_remedies`) reports crossings that
    recur, with partition membership as the remedy, in the `KernelEvent::Review` and the
    `kernel.review` record.
  - `adk-rust up` with `[[kernel.partitions]]` and `coordinator` in the user config runs one
    harness per partition (`adk-rust@<partition>`), with long-term memory on for partitions
    and a consolidation tick per partition in Ask or Auto that distils only that partition's
    sessions; `adk-rust partitions [list|add|set-mode P MODE]` lists and changes the table
    (`adk-cli`).
  - `examples/harness_kernel`: K7 scripted (`k7_partitions`) and live through `adk-rust up`
    (`live_k7_partitions`).
  - `KernelBuilder::partition_progress_source`: each partition's own progress source, which
    the kernel asks for its processes' goal progress, loop-guard verdicts and usage and hands
    the process table and the ledger; `host_partition` registers each partition's harness, so a
    partition's harness writes its ledger rows in its partition rather than `default`. Two
    sources for one partition are refused at `build`.
  - `/proc` observations per partition: a partition's harness is the audience of its own
    partition's observations only, and remembers a confirmed one in the partition's memory
    scope; `adk-rust up` registers each partition's harness for its partition.
  - The managed form: `KernelDef::coordinator` beside `partitions`, `PartitionDef::to_partition`,
    and one agent per partition on a tenant's kernel through
    `TenantKernelBuilder::partition_resolver` (`PartitionAgentResolver`, `PartitionHost`;
    `HarnessResolver` implements it) over the tenant's own partition table
    (`TenantKernelBuilder::partition_store`, `TenantKernel::partitions`,
    `TenantKernel::host_partitions`); `ManagedKernels` refuses a tenant whose partition store
    another tenant holds.
  - `PolicyCall::durable_run_id` (`adk-core`): the governed path tells a policy which durable
    run a call belongs to.
