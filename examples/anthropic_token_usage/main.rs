//! Anthropic Token Usage, Prompt Caching & Thinking Example
//!
//! Demonstrates three Anthropic features in one example:
//!
//! 1. **Token usage metadata** — prompt, candidate, and total token counts
//! 2. **Prompt caching** — `cache_read_input_token_count` and
//!    `cache_creation_input_token_count` when `with_prompt_caching(true)` is set.
//!    Anthropic caches system instructions and long prefixes, giving 90% discount
//!    on cache reads.
//! 3. **Extended thinking** — `Part::Thinking` blocks with Claude's internal
//!    reasoning when `with_thinking(budget)` is configured.
//!
//! ```bash
//! export ANTHROPIC_API_KEY=sk-ant-...
//! cargo run --example anthropic_token_usage --features anthropic
//! ```

use adk_core::{Content, Llm, LlmRequest, Part, UsageMetadata};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use futures::StreamExt;
use std::collections::HashMap;

fn print_usage(label: &str, usage: &UsageMetadata) {
    println!("--- {label} ---");
    println!("  prompt tokens:           {}", usage.prompt_token_count);
    println!("  candidate tokens:        {}", usage.candidates_token_count);
    println!("  total tokens:            {}", usage.total_token_count);
    if let Some(cache_read) = usage.cache_read_input_token_count {
        println!("  cache read tokens:       {cache_read}  ← 90% cheaper");
    }
    if let Some(cache_create) = usage.cache_creation_input_token_count {
        println!("  cache creation tokens:   {cache_create}  ← 25% surcharge on first use");
    }
    if let Some(thinking) = usage.thinking_token_count {
        println!("  thinking tokens:         {thinking}");
    }
    println!();
}

/// Long reference document for caching demo.
const REFERENCE_DOC: &str = r#"
# Software Architecture Patterns Reference

## 1. Microservices Architecture
Microservices decompose applications into small, independently deployable services.
Each service owns its data, communicates via APIs, and can be scaled independently.
Key principles: single responsibility, loose coupling, independent deployment,
decentralized data management, infrastructure automation, design for failure.

Benefits: independent scaling, technology diversity, fault isolation, team autonomy.
Challenges: distributed system complexity, data consistency, network latency,
operational overhead, testing complexity.

Common patterns: API Gateway, Service Discovery, Circuit Breaker, Saga Pattern,
Event Sourcing, CQRS, Sidecar, Ambassador, Strangler Fig.

## 2. Event-Driven Architecture
Systems communicate through events — immutable records of state changes.
Producers emit events without knowing consumers. Enables loose coupling and
real-time processing.

Components: Event Producers, Event Channels (topics/queues), Event Consumers,
Event Store. Patterns: Event Notification, Event-Carried State Transfer,
Event Sourcing, CQRS.

Technologies: Apache Kafka, RabbitMQ, AWS EventBridge, Azure Event Grid,
Google Cloud Pub/Sub, NATS, Redis Streams.

## 3. Domain-Driven Design (DDD)
Aligns software design with business domains. Core concepts: Bounded Contexts,
Aggregates, Entities, Value Objects, Domain Events, Repositories, Services.

Strategic patterns: Context Mapping, Shared Kernel, Anti-Corruption Layer,
Open Host Service, Published Language, Conformist, Customer-Supplier.

Tactical patterns: Aggregate Root, Domain Service, Application Service,
Infrastructure Service, Factory, Specification.

## 4. Clean Architecture
Dependency rule: dependencies point inward. Layers from inside out:
Entities (enterprise rules), Use Cases (application rules), Interface Adapters
(controllers, presenters, gateways), Frameworks & Drivers (web, DB, UI).

Benefits: testability, independence from frameworks/UI/database, flexibility.
Related: Hexagonal Architecture (Ports & Adapters), Onion Architecture.

## 5. Reactive Systems
Systems that are Responsive, Resilient, Elastic, and Message-Driven.
Based on the Reactive Manifesto. Implementation patterns: Actor Model,
Reactive Streams, Backpressure, Circuit Breaker, Bulkhead.

Technologies: Akka, Project Reactor, RxJava, Vert.x, Quarkus.

## 6. Serverless Architecture
Functions as a Service (FaaS) — code runs in stateless containers triggered
by events. No server management. Pay per execution.

Patterns: Function Composition, Fan-out/Fan-in, Async Messaging,
Event-driven Processing, Scheduled Tasks, API Backend.

Considerations: cold starts, execution limits, vendor lock-in, debugging
complexity, state management, cost at scale.

## 7. CQRS and Event Sourcing
Command Query Responsibility Segregation separates read and write models.
Event Sourcing stores state as a sequence of events rather than current state.

Benefits: optimized read/write models, audit trail, temporal queries,
event replay, scalability. Challenges: eventual consistency, complexity,
event schema evolution, snapshot management.

## 8. Service Mesh
Infrastructure layer for service-to-service communication. Handles:
load balancing, service discovery, encryption, observability, traffic management.

Components: Data Plane (sidecar proxies), Control Plane (configuration).
Technologies: Istio, Linkerd, Consul Connect, AWS App Mesh.
"#;

// ---------------------------------------------------------------------------
// Part 1: Prompt caching
// ---------------------------------------------------------------------------

async fn demo_prompt_caching(api_key: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Part 1: Prompt Caching ===\n");

    let config = AnthropicConfig::new(api_key, "claude-sonnet-4-20250514")
        .with_prompt_caching(true)
        .with_max_tokens(1024);

    let client = AnthropicClient::new(config)?;

    let system_prompt = format!(
        "You are a software architecture expert. Answer questions using ONLY the \
         reference material below. Cite the section number.\n\n{REFERENCE_DOC}"
    );

    // Request 1 — cache creation
    println!("  Request 1 (cache creation expected):\n");
    let request = LlmRequest {
        model: String::new(),
        contents: vec![
            Content::new("system").with_text(&system_prompt),
            Content::new("user").with_text("What is the dependency rule in Clean Architecture?"),
        ],
        config: None,
        tools: HashMap::new(),
    };

    let mut stream = client.generate_content(request, true).await?;
    let mut text = String::new();
    let mut final_usage = None;

    while let Some(result) = stream.next().await {
        let response = result?;
        if let Some(content) = &response.content {
            for part in &content.parts {
                if let Part::Text { text: t } = part {
                    text.push_str(t);
                }
            }
        }
        if response.turn_complete {
            final_usage = response.usage_metadata;
        }
    }

    let preview = &text[..text.len().min(200)];
    println!("  Response: {preview}...\n");
    if let Some(usage) = &final_usage {
        print_usage("Request 1 (cache creation)", usage);
    }

    // Request 2 — cache hit expected (same system prompt)
    println!("  Request 2 (cache hit expected):\n");
    let request = LlmRequest {
        model: String::new(),
        contents: vec![
            Content::new("system").with_text(&system_prompt),
            Content::new("user").with_text("Compare microservices and serverless architectures."),
        ],
        config: None,
        tools: HashMap::new(),
    };

    let mut stream = client.generate_content(request, true).await?;
    text.clear();
    final_usage = None;

    while let Some(result) = stream.next().await {
        let response = result?;
        if let Some(content) = &response.content {
            for part in &content.parts {
                if let Part::Text { text: t } = part {
                    text.push_str(t);
                }
            }
        }
        if response.turn_complete {
            final_usage = response.usage_metadata;
        }
    }

    let preview = &text[..text.len().min(200)];
    println!("  Response: {preview}...\n");
    if let Some(usage) = &final_usage {
        print_usage("Request 2 (cache hit)", usage);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Part 2: Extended thinking
// ---------------------------------------------------------------------------

async fn demo_thinking(api_key: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Part 2: Extended Thinking ===\n");

    let config = AnthropicConfig::new(api_key, "claude-sonnet-4-20250514")
        .with_max_tokens(16384)
        .with_thinking(8192);

    let client = AnthropicClient::new(config)?;

    let request = LlmRequest {
        model: String::new(),
        contents: vec![Content::new("user").with_text(
            "There are three boxes. One contains only apples, one contains only oranges, \
             and one contains both. The boxes are labeled, but ALL labels are wrong. \
             You can pick one fruit from one box. How do you determine what's in each box?",
        )],
        config: None,
        tools: HashMap::new(),
    };

    println!("  Question: Mislabeled boxes puzzle\n");

    let mut stream = client.generate_content(request, true).await?;
    let mut thinking_count = 0;

    while let Some(result) = stream.next().await {
        let response = result?;
        if let Some(content) = &response.content {
            for part in &content.parts {
                match part {
                    Part::Thinking { thinking, .. } => {
                        thinking_count += 1;
                        let preview = &thinking[..thinking.len().min(120)];
                        println!("  💭 Thinking #{thinking_count}: {preview}...");
                    }
                    Part::Text { text } => print!("{text}"),
                    _ => {}
                }
            }
        }
        if response.turn_complete {
            println!();
            if let Some(usage) = &response.usage_metadata {
                print_usage("Thinking request", usage);
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");

    println!("=== Anthropic Token Usage, Caching & Thinking Demo ===\n");

    demo_prompt_caching(&api_key).await?;
    demo_thinking(&api_key).await?;

    println!("=== Key Takeaways ===");
    println!("• with_prompt_caching(true) enables cache_control on system instructions");
    println!("• cache_creation tokens have a 25% surcharge on first use");
    println!("• cache_read tokens get a 90% discount on subsequent requests");
    println!("• with_thinking(budget) enables extended thinking with Part::Thinking blocks");
    println!("• Thinking blocks show Claude's internal reasoning process");

    Ok(())
}
