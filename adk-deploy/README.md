# adk-deploy

Deployment manifest, bundling, and control-plane client for ADK-Rust agents.

## Features

- Deployment manifest parsing and validation (TOML-based)
- Agent bundling with SHA-256 integrity checksums
- Compressed archive creation (tar.gz)
- Control-plane HTTP client for push-based deployments

## Installation

```toml
[dependencies]
adk-deploy = "0.4.1"
```

## License

Apache-2.0
