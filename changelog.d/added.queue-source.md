- **Queue wake-ups** (`adk-core`): the `QueueSource` trait (pull, ack, nack),
  `InMemoryQueueSource`, and `QueueIngress`, which enqueues each message as a durable run keyed
  by `queue:{source}:{message id}` and acks only after the enqueue succeeds. Governance and a
  `max_pending` cap hold pulls. `queue_source_conformance_tests!` (feature `conformance`)
  checks broker adapters.
