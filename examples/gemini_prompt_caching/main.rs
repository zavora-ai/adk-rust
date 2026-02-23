//! Gemini Prompt Caching Lifecycle Example
//!
//! Demonstrates automatic prompt caching with the ADK runner. When a long
//! system instruction is reused across multiple turns, the runner can cache
//! it server-side via the Gemini Caching API, reducing latency and cost.
//!
//! This example shows:
//! - Configuring `ContextCacheConfig` on the runner
//! - Observing `cache_read_input_token_count` in usage metadata
//! - Using `CachePerformanceAnalyzer` to compute hit ratios
//!
//! Requires a Gemini model that supports caching (gemini-2.0-flash or later).
//! The system instruction must exceed `min_tokens` (default 4096) for caching
//! to activate.
//!
//! ```bash
//! export GOOGLE_API_KEY=...
//! cargo run --example gemini_prompt_caching
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, ContextCacheConfig, Part};
use adk_model::gemini::GeminiModel;
use adk_runner::{CachePerformanceAnalyzer, Runner, RunnerConfig};
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

/// A long reference document that will be cached server-side.
/// Must be large enough to exceed the `min_tokens` threshold.
const REFERENCE_DOC: &str = r#"
# Comprehensive API Reference: Acme Cloud Platform v3.0

## Chapter 1: Authentication and Authorization

### 1.1 OAuth 2.0 Flow
The Acme Cloud Platform uses OAuth 2.0 for all API authentication. Clients must
obtain an access token before making any API calls. The supported grant types are:

- **Authorization Code**: For server-side applications with user interaction.
  Redirect the user to `https://auth.acme.cloud/authorize` with parameters:
  `client_id`, `redirect_uri`, `response_type=code`, `scope`.
  Exchange the authorization code at `https://auth.acme.cloud/token`.

- **Client Credentials**: For service-to-service communication without user context.
  POST to `https://auth.acme.cloud/token` with `grant_type=client_credentials`,
  `client_id`, and `client_secret`.

- **Device Code**: For devices with limited input capabilities.
  POST to `https://auth.acme.cloud/device/code` to obtain a device code,
  then poll `https://auth.acme.cloud/token` until the user authorizes.

### 1.2 API Keys
For read-only public endpoints, API keys can be used instead of OAuth tokens.
Include the key as a query parameter `?api_key=YOUR_KEY` or in the header
`X-API-Key: YOUR_KEY`. API keys are rate-limited to 100 requests per minute.

### 1.3 Scopes
Available OAuth scopes:
- `read:resources` — Read access to all resources
- `write:resources` — Create and update resources
- `delete:resources` — Delete resources
- `admin:platform` — Full administrative access
- `read:analytics` — Access to analytics and reporting
- `write:webhooks` — Manage webhook subscriptions

### 1.4 Token Refresh
Access tokens expire after 3600 seconds. Use the refresh token to obtain a new
access token without user interaction. POST to `https://auth.acme.cloud/token`
with `grant_type=refresh_token` and `refresh_token=YOUR_REFRESH_TOKEN`.

## Chapter 2: Resource Management

### 2.1 Projects
Projects are the top-level organizational unit. Each project has:
- A unique `project_id` (auto-generated UUID)
- A human-readable `name` (3-64 characters, alphanumeric and hyphens)
- An `owner_id` referencing the creating user
- A `billing_account_id` for cost attribution
- `labels` — key-value pairs for organization (max 64 labels)
- `created_at` and `updated_at` timestamps (ISO 8601)

API Endpoints:
- `GET /v3/projects` — List all projects (paginated, max 100 per page)
- `POST /v3/projects` — Create a new project
- `GET /v3/projects/{project_id}` — Get project details
- `PATCH /v3/projects/{project_id}` — Update project metadata
- `DELETE /v3/projects/{project_id}` — Delete project (requires confirmation)

### 2.2 Environments
Each project can have multiple environments (e.g., development, staging, production).
Environments provide isolated configuration and resource namespaces.

Fields:
- `environment_id` — UUID
- `project_id` — Parent project reference
- `name` — Environment name (e.g., "production")
- `tier` — One of: `free`, `standard`, `premium`, `enterprise`
- `region` — Deployment region (e.g., "us-east-1", "eu-west-1", "ap-southeast-1")
- `config` — Environment-specific configuration object
- `status` — One of: `active`, `suspended`, `decommissioning`

API Endpoints:
- `GET /v3/projects/{project_id}/environments` — List environments
- `POST /v3/projects/{project_id}/environments` — Create environment
- `GET /v3/projects/{project_id}/environments/{env_id}` — Get details
- `PATCH /v3/projects/{project_id}/environments/{env_id}` — Update
- `DELETE /v3/projects/{project_id}/environments/{env_id}` — Delete

### 2.3 Services
Services are deployable units within an environment. Each service runs as a
container with configurable resources.

Fields:
- `service_id` — UUID
- `name` — Service name (must be unique within environment)
- `image` — Container image reference (e.g., "registry.acme.cloud/myapp:v1.2")
- `replicas` — Number of instances (1-100)
- `cpu` — CPU allocation in millicores (100-8000)
- `memory` — Memory allocation in MiB (128-32768)
- `ports` — Array of exposed ports with protocol (TCP/UDP/HTTP)
- `env_vars` — Environment variables (secrets referenced by name)
- `health_check` — Health check configuration (path, interval, timeout)
- `auto_scaling` — Auto-scaling rules (min, max, target CPU/memory percentage)

API Endpoints:
- `GET /v3/projects/{pid}/environments/{eid}/services` — List services
- `POST /v3/projects/{pid}/environments/{eid}/services` — Deploy service
- `GET /v3/projects/{pid}/environments/{eid}/services/{sid}` — Get details
- `PATCH /v3/projects/{pid}/environments/{eid}/services/{sid}` — Update
- `DELETE /v3/projects/{pid}/environments/{eid}/services/{sid}` — Remove
- `POST /v3/projects/{pid}/environments/{eid}/services/{sid}/restart` — Restart

## Chapter 3: Networking

### 3.1 Load Balancers
Each environment gets a managed load balancer. Configuration options:
- `algorithm` — Round-robin, least-connections, or IP-hash
- `ssl_certificate` — TLS certificate reference for HTTPS termination
- `custom_domains` — Array of custom domain names with DNS verification
- `rate_limiting` — Requests per second per client IP (default: 1000)
- `cors` — Cross-origin resource sharing configuration
- `websocket_support` — Enable WebSocket upgrade (default: true)

### 3.2 DNS Management
Managed DNS with automatic record creation for services:
- A/AAAA records for load balancer IPs
- CNAME records for custom domains
- TXT records for domain verification
- SRV records for service discovery
- TTL configuration (minimum 60 seconds)

### 3.3 VPC and Private Networking
Enterprise tier includes VPC support:
- Private subnets with CIDR block allocation
- VPC peering with external cloud providers
- Transit gateway for multi-VPC connectivity
- Network ACLs and security groups
- VPN gateway for on-premises connectivity

## Chapter 4: Storage

### 4.1 Object Storage
S3-compatible object storage with:
- Bucket creation and management
- Object versioning
- Lifecycle policies (transition, expiration)
- Cross-region replication
- Server-side encryption (AES-256, KMS)
- Pre-signed URLs for temporary access
- Multipart upload for large objects (up to 5 TB)

### 4.2 Block Storage
Persistent block volumes for services:
- Volume types: `ssd` (IOPS-optimized), `hdd` (throughput-optimized)
- Sizes: 1 GiB to 16 TiB
- Snapshots and restore
- Volume encryption
- Automatic backup schedules

### 4.3 Database Services
Managed database offerings:
- PostgreSQL 14, 15, 16
- MySQL 8.0
- Redis 7.x (caching and pub/sub)
- MongoDB 7.0
- Connection pooling (PgBouncer for PostgreSQL)
- Automated backups (daily, configurable retention 1-35 days)
- Read replicas (up to 5 per primary)
- Point-in-time recovery

## Chapter 5: Monitoring and Observability

### 5.1 Metrics
Built-in metrics collection:
- CPU utilization, memory usage, network I/O
- Request count, latency (p50, p95, p99), error rate
- Custom metrics via StatsD or Prometheus exposition format
- Metric retention: 15 months at decreasing resolution
- Alerting rules with notification channels (email, Slack, PagerDuty)

### 5.2 Logging
Centralized log aggregation:
- Structured JSON logging recommended
- Log levels: DEBUG, INFO, WARN, ERROR, FATAL
- Full-text search with Lucene query syntax
- Log retention: 30 days (standard), 90 days (premium), 365 days (enterprise)
- Log-based metrics and alerts
- Export to external systems (S3, Elasticsearch, Splunk)

### 5.3 Tracing
Distributed tracing with OpenTelemetry:
- Automatic instrumentation for HTTP and gRPC
- Custom span creation via SDK
- Trace sampling (head-based and tail-based)
- Service dependency maps
- Latency breakdown by service and operation

## Chapter 6: Security

### 6.1 Identity and Access Management
Role-based access control (RBAC):
- Predefined roles: Viewer, Editor, Admin, Owner
- Custom roles with fine-grained permissions
- Service accounts for automated workflows
- Multi-factor authentication (TOTP, WebAuthn)
- SSO integration (SAML 2.0, OIDC)

### 6.2 Secrets Management
Secure storage for sensitive configuration:
- AES-256 encryption at rest
- Automatic rotation policies
- Version history with rollback
- Access audit logging
- Integration with external vaults (HashiCorp Vault, AWS KMS)

### 6.3 Compliance
Compliance certifications and features:
- SOC 2 Type II
- ISO 27001
- GDPR data processing agreement
- HIPAA BAA (enterprise tier)
- Data residency controls by region
- Audit log retention (minimum 7 years for enterprise)
"#;

/// Run a single turn and print the response with usage metadata.
async fn ask(
    runner: &Runner,
    session_id: &str,
    question: &str,
    turn: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    println!(">> Turn {turn}: {question}\n");

    let content = Content::new("user").with_text(question);
    let mut stream = runner.run("user_1".to_string(), session_id.to_string(), content).await?;

    print!("   Assistant: ");
    let mut last_usage = None;
    while let Some(event) = stream.next().await {
        if let Ok(e) = event {
            if let Some(content) = e.llm_response.content {
                for part in content.parts {
                    if let Part::Text { text } = part {
                        print!("{text}");
                    }
                }
            }
            if e.llm_response.usage_metadata.is_some() {
                last_usage = e.llm_response.usage_metadata;
            }
        }
    }
    println!("\n");

    if let Some(usage) = &last_usage {
        println!("   Token usage:");
        println!("     prompt:         {}", usage.prompt_token_count);
        println!("     candidates:     {}", usage.candidates_token_count);
        println!("     total:          {}", usage.total_token_count);
        if let Some(cache_read) = usage.cache_read_input_token_count {
            println!("     cache read:     {cache_read}  ← tokens served from cache");
        }
        if let Some(cache_create) = usage.cache_creation_input_token_count {
            println!("     cache created:  {cache_create}  ← tokens used to populate cache");
        }
        if let Some(thinking) = usage.thinking_token_count {
            println!("     thinking:       {thinking}");
        }
    }
    println!();

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    let instruction = format!(
        "You are a technical support agent for the Acme Cloud Platform. \
         Answer questions using ONLY the reference documentation below. \
         Cite the relevant section number when answering.\n\n\
         {REFERENCE_DOC}"
    );

    let agent = LlmAgentBuilder::new("acme_support")
        .model(model.clone())
        .instruction(instruction)
        .build()?;

    let session_service = Arc::new(InMemorySessionService::new());
    let session = session_service
        .create(CreateRequest {
            app_name: "gemini_prompt_caching".to_string(),
            user_id: "user_1".to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;
    let session_id = session.id().to_string();

    // Configure prompt caching:
    //   min_tokens: 1024 — cache if system instruction exceeds this
    //   ttl_seconds: 300  — cache lives for 5 minutes
    //   cache_intervals: 5 — refresh cache after 5 invocations
    let cache_config =
        ContextCacheConfig { min_tokens: 1024, ttl_seconds: 300, cache_intervals: 5 };

    let runner = Runner::new(RunnerConfig {
        app_name: "gemini_prompt_caching".to_string(),
        agent: Arc::new(agent),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: Some(cache_config),
        cache_capable: Some(model),
    })?;

    println!("=== Gemini Prompt Caching Lifecycle Demo ===\n");
    println!(
        "System instruction: ~{} words of API documentation",
        REFERENCE_DOC.split_whitespace().count()
    );
    println!("Cache config: min_tokens=1024, ttl=300s, intervals=5\n");
    println!("Watch for `cache read` tokens increasing after the first turn.\n");

    // Turn 1 — cache miss, populates cache
    ask(&runner, &session_id, "How do I authenticate with OAuth 2.0 client credentials?", 1)
        .await?;

    // Turn 2 — should see cache hits
    ask(&runner, &session_id, "What database services are available and what versions?", 2).await?;

    // Turn 3 — more cache hits
    ask(&runner, &session_id, "Explain the auto-scaling configuration for services.", 3).await?;

    // Analyze cache performance from session events
    let session = session_service
        .get(GetRequest {
            app_name: "gemini_prompt_caching".to_string(),
            user_id: "user_1".to_string(),
            session_id: session_id.clone(),
            num_recent_events: None,
            after: None,
        })
        .await?;

    let metrics = CachePerformanceAnalyzer::analyze(&session.events().all());

    println!("=== Cache Performance Metrics ===\n");
    println!("  total requests:              {}", metrics.total_requests);
    println!("  requests with cache hits:    {}", metrics.requests_with_cache_hits);
    println!("  total prompt tokens:         {}", metrics.total_prompt_tokens);
    println!("  total cache read tokens:     {}", metrics.total_cache_read_tokens);
    println!("  total cache creation tokens: {}", metrics.total_cache_creation_tokens);
    println!("  cache hit ratio:             {:.1}%", metrics.cache_hit_ratio);
    println!("  cache utilization ratio:     {:.1}%", metrics.cache_utilization_ratio);
    println!("  avg cached tokens/request:   {:.0}", metrics.avg_cached_tokens_per_request);
    println!();
    println!("Higher cache_hit_ratio means more tokens served from cache (cheaper and faster).");

    Ok(())
}
