# A2A Interceptors Example

Demonstrates ADK-Rust's A2A (Agent-to-Agent) interceptor chain for request processing, showing how to implement authentication, rate limiting, and audit logging as composable interceptors that inspect, modify, or reject requests before they reach the agent handler.

## What This Shows

- **`A2aInterceptor` trait** — implementing custom request processing logic with the `before_delegation` method that returns an `InterceptorDecision`
- **`InterceptorDecision::Continue`** — allowing a request to pass through to the next interceptor or the agent handler
- **`InterceptorDecision::Reject`** — denying a request with an error message, stopping the chain immediately
- **`InterceptorDecision::ShortCircuit`** — returning an immediate response (e.g., rate-limit error) without reaching the handler
- **`InterceptorChain`** — composing multiple interceptors into an ordered pipeline where each interceptor can halt processing
- **`BearerAuthInterceptor`** — validates bearer tokens against a known set, extracts client identity, rejects invalid credentials
- **`RateLimitInterceptor`** — tracks per-client request counts within a time window, short-circuits when the limit is exceeded
- **`AuditLogInterceptor`** — records method, client identity, timestamp, and duration for every request passing through the chain

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`GOOGLE_API_KEY`** environment variable set with a valid Gemini API key

Set up your environment:

```bash
cp examples/a2a_interceptors/.env.example examples/a2a_interceptors/.env
# Edit .env and add your GOOGLE_API_KEY
```

## Run

```bash
cargo run --manifest-path examples/a2a_interceptors/Cargo.toml
```

To enable debug logging:

```bash
RUST_LOG=debug cargo run --manifest-path examples/a2a_interceptors/Cargo.toml
```

## Expected Output

```
╔════════════════════════════════════════════╗
║  A2A Interceptors — ADK-Rust v1.0       ║
╚════════════════════════════════════════════╝

  ✓ GOOGLE_API_KEY loaded (39 chars)

--- Step 1: Configure A2A Server with Interceptor Chain ---

  ✓ InterceptorChain configured with 3 interceptors:
      1. BearerAuthInterceptor — validates bearer tokens
      2. RateLimitInterceptor — max 3 requests/client/60s
      3. AuditLogInterceptor  — records method, client, duration

--- Step 2: Send Request with Valid Bearer Token ---

  → Sending 'tasks/send' with bearer token: valid-token-abc
  ✓ Decision: Continue
  ✓ Interceptors executed: BearerAuthInterceptor → RateLimitInterceptor → AuditLogInterceptor
  ✓ Request passed all interceptors — proceeding to agent handler

--- Step 3: Send Request with Invalid Bearer Token ---

  → Sending 'tasks/send' with bearer token: invalid-token-xyz
  ⚠ Decision: Reject
  ⚠ Reason: Invalid bearer token: authentication failed for token 'invalid-'
  ✓ Interceptors executed: BearerAuthInterceptor
  → Chain stopped at BearerAuthInterceptor — RateLimitInterceptor and AuditLogInterceptor were skipped

--- Step 4: Exceed Rate Limit (ShortCircuit) ---

  → Sending 4 rapid requests from agent-client-1 (limit: 3/60s)

  ✓ Request #2: Continue (executed: BearerAuthInterceptor → RateLimitInterceptor → AuditLogInterceptor)
  ✓ Request #3: Continue (executed: BearerAuthInterceptor → RateLimitInterceptor → AuditLogInterceptor)
  ⚠ Request #4: ShortCircuit — rate limit exceeded!
  ⚠ Response: Rate limit exceeded for client 'agent-client-1': 4 requests in 60s (max: 3)
  ✓ Interceptors executed: BearerAuthInterceptor → RateLimitInterceptor

--- Step 5: Print Audit Log Entries ---

  ✓ 4 audit entries recorded:

  Duration   Method                 Client            Decision
  ──────────────────────────────────────────────────────────────────────────
  [   0.012ms] method=tasks/send          client=agent-client-1  decision=Continue
  [   0.008ms] method=tasks/send          client=unauthenticated decision=Reject(Invalid bearer token: ...)
  [   0.010ms] method=tasks/send#2        client=agent-client-1  decision=Continue
  [   0.009ms] method=tasks/send#3        client=agent-client-1  decision=Continue
  [   0.011ms] method=tasks/send#4        client=agent-client-1  decision=ShortCircuit

--- Summary ---

  Total requests processed: 5
    Continued (passed all interceptors): 3
    Rejected (auth failure):             1
    Short-circuited (rate limited):      1

  BearerAuthInterceptor: validates tokens, sets client identity, rejects invalid credentials
  RateLimitInterceptor: enforces per-client request limits, short-circuits on excess
  AuditLogInterceptor: records method, client identity, and duration for all requests
  InterceptorChain executes interceptors in order; early decisions skip remaining chain

✅ Example completed successfully.
```

## Key APIs Demonstrated

### A2aInterceptor Trait

Implement the `A2aInterceptor` trait to create custom request processing logic. Each interceptor inspects or modifies the request and returns a decision:

```rust
use adk_server::a2a::interceptor::{A2aInterceptor, InterceptorDecision};
use async_trait::async_trait;

struct BearerAuthInterceptor {
    valid_tokens: HashMap<String, String>,
}

#[async_trait]
impl A2aInterceptor for BearerAuthInterceptor {
    fn name(&self) -> &str {
        "BearerAuthInterceptor"
    }

    async fn before_delegation(
        &self,
        request: &mut A2aRequest,
    ) -> InterceptorDecision {
        match &request.bearer_token {
            None => InterceptorDecision::Reject {
                reason: "Missing Authorization header".to_string(),
            },
            Some(token) => {
                if let Some(client_id) = self.valid_tokens.get(token) {
                    request.client_id = Some(client_id.clone());
                    InterceptorDecision::Continue
                } else {
                    InterceptorDecision::Reject {
                        reason: "Invalid bearer token".to_string(),
                    }
                }
            }
        }
    }
}
```

### InterceptorDecision

The three possible outcomes from an interceptor:

```rust
enum InterceptorDecision {
    /// Allow the request to continue to the next interceptor or handler.
    Continue,

    /// Reject the request — no further interceptors are executed.
    Reject { reason: String },

    /// Short-circuit the chain and return a response immediately.
    /// Used for rate limiting or cached responses.
    ShortCircuit { response: Value },
}
```

### InterceptorChain

Compose multiple interceptors into an ordered pipeline. The chain executes each interceptor in sequence and stops at the first non-`Continue` decision:

```rust
use adk_server::a2a::interceptor::InterceptorChain;

let chain = InterceptorChain::new(vec![
    Arc::new(BearerAuthInterceptor::new(valid_tokens)),
    Arc::new(RateLimitInterceptor::new(3, Duration::from_secs(60))),
    Arc::new(AuditLogInterceptor::new(audit_log)),
]);

// Execute the chain on a request
let (decision, executed_interceptors) = chain.execute(&mut request).await;

match decision {
    InterceptorDecision::Continue => {
        // All interceptors passed — forward to agent handler
    }
    InterceptorDecision::Reject { reason } => {
        // Request denied — return error to client
    }
    InterceptorDecision::ShortCircuit { response } => {
        // Immediate response — e.g., rate limit error
    }
}
```

### RateLimitInterceptor with ShortCircuit

Rate limiting uses `ShortCircuit` to return an immediate 429-style response without reaching the agent:

```rust
struct RateLimitInterceptor {
    max_requests: usize,
    window: Duration,
    state: Arc<Mutex<HashMap<String, (usize, Instant)>>>,
}

#[async_trait]
impl A2aInterceptor for RateLimitInterceptor {
    async fn before_delegation(
        &self,
        request: &mut A2aRequest,
    ) -> InterceptorDecision {
        // Track per-client request count within the time window
        if count > self.max_requests {
            InterceptorDecision::ShortCircuit {
                response: json!({
                    "error": { "code": 429, "message": "Rate limit exceeded" }
                }),
            }
        } else {
            InterceptorDecision::Continue
        }
    }
}
```

### AuditLogInterceptor (Observe-Only)

An interceptor that always returns `Continue` but records metadata for every request:

```rust
struct AuditLogInterceptor {
    log: Arc<Mutex<Vec<AuditEntry>>>,
}

#[async_trait]
impl A2aInterceptor for AuditLogInterceptor {
    async fn before_delegation(
        &self,
        request: &mut A2aRequest,
    ) -> InterceptorDecision {
        let entry = AuditEntry {
            method: request.method.clone(),
            client_id: request.client_id.clone().unwrap_or_default(),
            timestamp: Instant::now(),
            duration: Duration::ZERO,
            decision: "pending".to_string(),
        };
        self.log.lock().await.push(entry);
        InterceptorDecision::Continue
    }
}
```
