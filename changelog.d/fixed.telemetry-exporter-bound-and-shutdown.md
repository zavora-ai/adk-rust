- **`AdkSpanExporter` retention is bounded** (`adk-telemetry`): the in-memory exporter
  keeps at most `DEFAULT_MAX_SPANS` (10,000) spans, evicting the least recently stored,
  instead of growing for the life of the process. `AdkSpanExporter::with_max_spans` and
  `AdkSpanExporter::with_ttl` change the bound and add an expiry.
- **`shutdown_telemetry` flushes the provider the tracing layer holds**
  (`adk-telemetry`): it force-flushes and shuts down every tracer and meter provider
  `init_with_otlp`, `build_otlp_layer`, and `init_with_gcp` built. It previously only
  replaced the global provider, and the layer's own reference kept the old one alive
  with its batch unexported.
