# Telemetry to Google Cloud

The `gcp` feature of `adk-telemetry` exports traces directly to Google Cloud Observability (Cloud Trace) and writes structured JSON logs that Cloud Logging parses natively — severity, message, and trace correlation included.

## Enable the Feature

```toml
[dependencies]
adk-telemetry = { version = "2.1.0", features = ["gcp"] }
```

Or through the umbrella crate (`gcp-telemetry` is also part of the `gemini-agent-platform` meta-feature):

```toml
[dependencies]
adk-rust = { version = "2.1.0", features = ["minimal", "gcp-telemetry"] }
```

## Direct Export to Google Cloud

`init_with_gcp` exports spans to `https://telemetry.googleapis.com` over gRPC, authenticating each request with a Bearer token minted from Application Default Credentials (ADC) plus an `x-goog-user-project` header. A background task re-mints the token every five minutes, so long-running agents keep exporting across token expiry.

```rust
use adk_telemetry::init_with_gcp;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Requires GOOGLE_CLOUD_PROJECT and Application Default Credentials.
    init_with_gcp("my-agent").await?;

    // Your agent code here

    adk_telemetry::shutdown_telemetry();
    Ok(())
}
```

Requirements:

| Requirement | How |
|-------------|-----|
| `GOOGLE_CLOUD_PROJECT` | Set to the project that receives (and is billed for) the telemetry |
| Credentials | `gcloud auth application-default login` locally, or an attached service account when deployed |
| API | Enable the Telemetry API (`telemetry.googleapis.com`) on the project |
| IAM | The principal needs the `telemetry.traces.write` permission (`roles/telemetry.tracesWriter`) |

> **Note:** This path exports traces only. Route metrics through the collector sidecar below.

## GCP Resource Detection

`init_with_gcp` derives OpenTelemetry resource attributes from the environment variables the platform sets in deployed containers:

| Attribute | Source |
|-----------|--------|
| `service.name` | `K_SERVICE`, else `GOOGLE_CLOUD_AGENT_ENGINE_ID`, else the `service_name` argument |
| `gcp.project_id` | `GOOGLE_CLOUD_PROJECT` (omitted when unset) |
| `cloud.platform` | `gcp.agent_engine` when `GOOGLE_CLOUD_AGENT_ENGINE_ID` is set |

`GOOGLE_CLOUD_AGENT_ENGINE_ID` is the bare numeric engine ID that Vertex AI Agent Engine sets in deployed containers. `gcp.agent_engine` is the canonical `cloud.platform` value from the OpenTelemetry semantic conventions (added upstream in October 2025).

The same detection is available standalone for composing your own pipeline:

```rust
use adk_telemetry::gcp_resource_attributes;

let attributes = gcp_resource_attributes("my-agent");
```

## Structured JSON Logging for Cloud Logging

`init_json_logging` writes one JSON object per line to stdout, so Cloud Logging parses severity and trace fields instead of showing opaque text payloads. `init_with_gcp` installs the same format automatically.

```rust
use adk_telemetry::init_json_logging;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_json_logging()?;

    tracing::info!(user.id = "u1", "request handled");
    Ok(())
}
```

Emitted fields:

| Field | Content |
|-------|---------|
| `timestamp` | RFC 3339 event time |
| `severity` | `DEBUG` / `INFO` / `WARNING` / `ERROR` (tracing `trace` and `debug` both map to `DEBUG`, `warn` maps to `WARNING`) |
| `message` | The event message |
| `target` | The tracing target |
| `logging.googleapis.com/trace` | `projects/{project}/traces/{trace_id}` from the active OpenTelemetry span |
| `logging.googleapis.com/spanId` | Span ID from the active OpenTelemetry span |
| `logging.googleapis.com/trace_sampled` | Sampling decision |
| *(event and span fields)* | Each field as a typed JSON value |

Trace correlation fields require an OpenTelemetry layer in the same subscriber and `GOOGLE_CLOUD_PROJECT` — `init_with_gcp` provides both, which makes log entries appear inline in the Cloud Trace panel.

## Fallback: OTLP to a Collector Sidecar

When direct API access is unavailable — or when you also need metrics — run an OpenTelemetry Collector as a sidecar and point the standard OTLP exporter at it:

```rust
use adk_telemetry::init_with_otlp;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_with_otlp("my-agent", "http://localhost:4317")?;

    // Your agent code here

    adk_telemetry::shutdown_telemetry();
    Ok(())
}
```

Collector configuration forwarding to Google Cloud (`otel-collector-config.yaml`, using the [googlecloud exporter](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/exporter/googlecloudexporter) from the contrib distribution):

```yaml
receivers:
  otlp:
    protocols:
      grpc:
        endpoint: 0.0.0.0:4317

processors:
  batch:
    send_batch_size: 200
    timeout: 5s
  resourcedetection:
    detectors: [env, gcp]

exporters:
  googlecloud:
    project: my-project

service:
  pipelines:
    traces:
      receivers: [otlp]
      processors: [resourcedetection, batch]
      exporters: [googlecloud]
    metrics:
      receivers: [otlp]
      processors: [resourcedetection, batch]
      exporters: [googlecloud]
```

Run it locally for development:

```bash
docker run --rm -p 4317:4317 \
  -v ./otel-collector-config.yaml:/etc/otelcol-contrib/config.yaml \
  -v ~/.config/gcloud:/root/.config/gcloud \
  otel/opentelemetry-collector-contrib:latest
```

## Choosing a Path

| Path | Signals | Extra infrastructure | Auth |
|------|---------|---------------------|------|
| `init_with_gcp` | Traces + JSON logs on stdout | None | ADC in-process |
| Collector sidecar | Traces + metrics (+ logs via collector) | Collector container | ADC in the collector |

## Related

- [Telemetry](telemetry.md) - Core telemetry setup and span helpers
- [Deployment](../deployment/server.md) - Production telemetry setup
