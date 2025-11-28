# Deployment Guide

Deploy ADK-Rust agents to production environments.

## Deployment Options

### 1. Standalone Binary

Build and deploy as a single executable.

```bash
# Build release binary
cargo build --release

# Binary location
./target/release/adk-cli

# Deploy
scp target/release/adk-cli user@server:/opt/my-agent/
```

**Pros**:
- Single file deployment
- No runtime dependencies
- Small binary size (~10-20MB)

**Cons**:
- Requires recompilation for updates
- Platform-specific builds needed

### 2. Docker Container

Containerize your agent for consistent deployment.

#### Dockerfile

```dockerfile
FROM rust:1.75 as builder

WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/my-agent /usr/local/bin/

ENV RUST_LOG=info
ENV PORT=8080

EXPOSE 8080

CMD ["my-agent", "serve"]
```

#### Build and Run

```bash
# Build image
docker build -t my-agent:latest .

# Run container
docker run -d \
  -p 8080:8080 \
  -e GOOGLE_API_KEY=$GOOGLE_API_KEY \
  -e RUST_LOG=info \
  --name my-agent \
  my-agent:latest

# View logs
docker logs -f my-agent
```

**Pros**:
- Consistent environment
- Easy scaling
- Version control

**Cons**:
- Larger image size
- Docker overhead

### 3. Kubernetes

Deploy to Kubernetes for production-grade orchestration.

#### deployment.yaml

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: adk-agent
spec:
  replicas: 3
  selector:
    matchLabels:
      app: adk-agent
  template:
    metadata:
      labels:
        app: adk-agent
    spec:
      containers:
      - name: agent
        image: my-agent:latest
        ports:
        - containerPort: 8080
        env:
        - name: GOOGLE_API_KEY
          valueFrom:
            secretKeyRef:
              name: api-keys
              key: google-api-key
        - name: DATABASE_URL
          value: "sqlite:///data/sessions.db"
        - name: RUST_LOG
          value: "info"
        resources:
          requests:
            memory: "256Mi"
            cpu: "250m"
          limits:
            memory: "512Mi"
            cpu: "500m"
        livenessProbe:
          httpGet:
            path: /health
            port: 8080
          initialDelaySeconds: 10
          periodSeconds: 30
        readinessProbe:
          httpGet:
            path: /health
            port: 8080
          initialDelaySeconds: 5
          periodSeconds: 10
---
apiVersion: v1
kind: Service
metadata:
  name: adk-agent
spec:
  selector:
    app: adk-agent
  ports:
  - port: 80
    targetPort: 8080
  type: LoadBalancer
```

#### Deploy

```bash
# Create secret for API keys
kubectl create secret generic api-keys \
  --from-literal=google-api-key=$GOOGLE_API_KEY

# Apply deployment
kubectl apply -f deployment.yaml

# Check status
kubectl get pods
kubectl get svc

# View logs
kubectl logs -f deployment/adk-agent
```

### 4. Serverless (AWS Lambda)

Deploy as serverless function (requires custom runtime).

#### lambda/bootstrap

```bash
#!/bin/sh
set -e
exec /var/task/my-agent lambda
```

#### Build for Lambda

```bash
# Use cargo-lambda
cargo install cargo-lambda

# Build
cargo lambda build --release

# Deploy
cargo lambda deploy my-agent
```

## Configuration Management

### Environment Variables

Recommended approach for configuration:

```bash
# Required
export GOOGLE_API_KEY="your-key"

# Optional
export PORT=8080
export DATABASE_URL="sqlite://sessions.db"
export RUST_LOG="info,adk_runner=debug"
export MAX_TOKENS=8192
export TEMPERATURE=0.7
```

### Configuration Files

Create `config.toml`:

```toml
[server]
host = "0.0.0.0"
port = 8080

[model]
name = "gemini-2.0-flash-exp"
max_tokens = 8192
temperature = 0.7

[database]
url = "sqlite://sessions.db"

[logging]
level = "info"
```

Load in code:

```rust
use config::Config;

let settings = Config::builder()
    .add_source(config::File::with_name("config"))
    .add_source(config::Environment::with_prefix("APP"))
    .build()?;
```

### Secrets Management

Never commit secrets to version control.

#### Using Environment Variables

```bash
# .env file (gitignored)
GOOGLE_API_KEY=your-key-here
DATABASE_PASSWORD=secure-password
```

Load with `dotenv`:

```rust
use dotenv::dotenv;

dotenv().ok();
let api_key = std::env::var("GOOGLE_API_KEY")?;
```

#### Using HashiCorp Vault

```rust
use vaultrs::client::{VaultClient, VaultClientSettingsBuilder};

let client = VaultClient::new(
    VaultClientSettingsBuilder::default()
        .address("https://vault.example.com")
        .token("vault-token")
        .build()?
)?;

let secret: String = client
    .read("secret/data/api-keys")
    .await?;
```

#### Using AWS Secrets Manager

```rust
use aws_sdk_secretsmanager as secretsmanager;

let config = aws_config::load_from_env().await;
let client = secretsmanager::Client::new(&config);

let secret = client
    .get_secret_value()
    .secret_id("my-agent/google-api-key")
    .send()
    .await?;
```

## Database Setup

### SQLite (Development/Small Scale)

```bash
# Initialize database
export DATABASE_URL="sqlite://sessions.db"

# Migrations (if using)
sqlx migrate run
```

**Pros**: Simple, no external dependencies  
**Cons**: Not suitable for high concurrency

### PostgreSQL (Production)

```bash
# Connection string
export DATABASE_URL="postgresql://user:password@localhost/adk_sessions"
```

Update session service:

```rust
use adk_session::DatabaseSessionService;

let session_service = Arc::new(
    DatabaseSessionService::new(&database_url).await?
);
```

### Redis (Caching/Sessions)

For high-performance session storage:

```rust
use redis::AsyncCommands;

let client = redis::Client::open("redis://127.0.0.1/")?;
let mut con = client.get_async_connection().await?;

// Store session
con.set_ex("session:123", json_data, 3600).await?;
```

## Monitoring & Observability

### Logging

ADK-Rust uses `tracing` for structured logging.

```rust
use tracing::{info, warn, error};
use tracing_subscriber;

// Initialize logging
tracing_subscriber::fmt()
    .with_env_filter("info,adk_runner=debug")
    .json()  // JSON format for log aggregation
    .init();

// In code
info!("Agent started: {}", agent_name);
warn!("High latency detected: {}ms", duration);
error!("Agent error: {}", err);
```

### Metrics

Expose Prometheus metrics:

```rust
use prometheus::{Registry, Counter, Histogram};

let registry = Registry::new();

let requests = Counter::new("agent_requests_total", "Total requests")?;
let latency = Histogram::new("agent_latency_seconds", "Request latency")?;

registry.register(Box::new(requests.clone()))?;
registry.register(Box::new(latency.clone()))?;

// Expose /metrics endpoint
use axum::{Router, routing::get};

let app = Router::new()
    .route("/metrics", get(metrics_handler));
```

### Distributed Tracing

OpenTelemetry integration:

```rust
use opentelemetry::{global, sdk::trace::Tracer};
use tracing_opentelemetry::OpenTelemetryLayer;

let tracer = /* configure tracer */;

tracing_subscriber::registry()
    .with(OpenTelemetryLayer::new(tracer))
    .init();
```

### Health Checks

Implement health endpoint:

```rust
use axum::{Json, routing::get};
use serde_json::json;

async fn health_check() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime": get_uptime(),
    }))
}

let app = Router::new()
    .route("/health", get(health_check));
```

## Performance Optimization

### 1. Connection Pooling

Reuse model connections:

```rust
use std::sync::Arc;

// Create once, share across agents
let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

// Use in multiple agents
let agent1 = LlmAgentBuilder::new("a1").model(model.clone()).build()?;
let agent2 = LlmAgentBuilder::new("a2").model(model.clone()).build()?;
```

### 2. Caching

Cache expensive operations:

```rust
use moka::future::Cache;

let cache: Cache<String, String> = Cache::new(10_000);

// Check cache first
if let Some(result) = cache.get(&key).await {
    return Ok(result);
}

// Compute and cache
let result = expensive_operation().await?;
cache.insert(key, result.clone()).await;
```

### 3. Rate Limiting

Protect against abuse:

```rust
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};

let config = GovernorConfigBuilder::default()
    .per_second(10)  // 10 requests per second
    .burst_size(20)
    .finish()
    .unwrap();

let app = Router::new()
    .route("/api/run", post(run_handler))
    .layer(GovernorLayer { config: Arc::new(config) });
```

### 4. Async I/O

Use async throughout:

```rust
#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    // ...
}
```

## Security

### 1. API Key Protection

Never expose API keys:

```rust
// ❌ Bad
let model = GeminiModel::new("hardcoded-key", "model")?;

// ✅ Good
let api_key = std::env::var("GOOGLE_API_KEY")?;
let model = GeminiModel::new(&api_key, "model")?;
```

### 2. Input Validation

Validate all user input:

```rust
fn validate_input(text: &str) -> Result<()> {
    if text.len() > 10_000 {
        return Err(AdkError::Agent("Input too long".into()));
    }
    if text.trim().is_empty() {
        return Err(AdkError::Agent("Empty input".into()));
    }
    Ok(())
}
```

### 3. Rate Limiting

Limit requests per user:

```rust
use std::collections::HashMap;
use std::sync::Mutex;

struct RateLimiter {
    limits: Mutex<HashMap<String, usize>>,
}

impl RateLimiter {
    fn check(&self, user_id: &str) -> bool {
        let mut limits = self.limits.lock().unwrap();
        let count = limits.entry(user_id.to_string()).or_insert(0);
        *count += 1;
        *count <= 100  // Max 100 requests
    }
}
```

### 4. HTTPS Only

Always use TLS in production:

```bash
# Use reverse proxy (nginx, traefik)
# or configure TLS in Axum
```

## Scaling

### Horizontal Scaling

Run multiple instances behind a load balancer:

```
Load Balancer
   ├─ Agent Instance 1
   ├─ Agent Instance 2
   └─ Agent Instance 3
```

Ensure:
- Stateless design (or shared state storage)
- Database can handle connections
- Consistent configuration

### Vertical Scaling

Increase resources per instance:

```yaml
resources:
  requests:
    memory: "1Gi"
    cpu: "1000m"
  limits:
    memory: "2Gi"
    cpu: "2000m"
```

### Auto-Scaling

Kubernetes HPA:

```yaml
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: adk-agent
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: adk-agent
  minReplicas: 2
  maxReplicas: 10
  metrics:
  - type: Resource
    resource:
      name: cpu
      target:
        type: Utilization
        averageUtilization: 70
```

## Troubleshooting

### High Memory Usage

Monitor and limit:

```rust
// Set reasonable limits
let config = GenerateContentConfig {
    max_output_tokens: Some(2048),
    ..Default::default()
};
```

### Slow Responses

Optimize:
- Use streaming for immediate feedback
- Cache frequent queries
- Reduce sequential agent chains
- Use parallel agents where possible

### Database Locks

Switch to PostgreSQL or use connection pooling:

```rust
use sqlx::postgres::PgPoolOptions;

let pool = PgPoolOptions::new()
    .max_connections(5)
    .connect(&database_url)
    .await?;
```

## Production Checklist

- [ ] API keys secured (env vars or secrets manager)
- [ ] HTTPS enabled
- [ ] Logging configured (JSON format)
- [ ] Metrics exposed (/metrics endpoint)
- [ ] Health checks implemented
- [ ] Rate limiting enabled
- [ ] Input validation implemented
- [ ] Database migrations applied
- [ ] Backups configured
- [ ] Monitoring/alerting set up
- [ ] Load testing completed
- [ ] Error handling tested
- [ ] Documentation updated

---

**Previous**: [Workflow Patterns](07_workflows.md) | **Next**: [CLI Usage](09_cli.md)
