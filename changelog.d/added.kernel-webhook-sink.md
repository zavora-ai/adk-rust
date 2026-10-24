- **Outbound webhook sink on the kernel's event bus** (`adk-kernel`, spec 3.2 §7.6):
  `WebhookSink` posts the events an `EventFilter` selects (by default every type but
  `health`) to an `https` endpoint, or plain `http` on loopback, signed with Standard
  Webhooks signatures under one `webhook-id` per event. Deliveries retry under a
  `RetryPolicy`, end undeliverable events at a `DeadLetterSink`, and run at most
  `with_max_in_flight` at once, so a slow receiver never blocks the bus. `EventKind`,
  `KernelEvent::kind` and `EventSink::emit_record` (the event with its cursor) are new.
