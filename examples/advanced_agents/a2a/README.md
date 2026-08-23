# A2A exposure

`a2a_gateway` is the server root and is exposed through both the normal runtime
API and ADK-Rust's A2A endpoints:

- `GET /.well-known/agent.json`
- `POST /a2a`
- `POST /a2a/stream`

Open the Runtime's **Protocols** tab to inspect and follow the discovered agent
card. This proves that the UI and A2A execute the same agent object, model,
session service, callbacks, and telemetry configuration.

Example request:

```bash
curl http://127.0.0.1:8088/.well-known/agent.json | jq .
```
