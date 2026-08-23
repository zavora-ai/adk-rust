# Telemetry and runtime services

The advanced runtime initializes `AdkSpanExporter` and configures in-memory
artifact and memory services. Agent details report whether each service is
actually configured. Telemetry reports `configured` before the first retained
span and `collecting` after the runtime captures one; console `RUST_LOG` levels
do not disable this collection.

After any interactive, ambient, realtime, A2A, or MCP-backed run:

1. open **Telemetry** for session spans, durations, trace IDs, and attributes;
2. open **Timeline** for ordered ADK events;
3. open **State** and **Artifacts** for their corresponding session data;
4. open **Protocols** for server-service and protocol availability.

Separating telemetry from the event timeline keeps spans visible even for runs
with many streaming or tool events.
