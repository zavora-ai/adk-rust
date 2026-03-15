use adk_core::{Content, Part, Result};
use adk_memory::{
    EmbeddingProvider, MemoryEntry, MemoryService, PostgresMemoryService, SearchRequest,
};
use async_trait::async_trait;
use chrono::Utc;

const DATABASE_URL: &str = "postgres://adk:adk_test@localhost:5498/adk_memory";

/// A mock embedding provider that produces deterministic vectors from text.
///
/// Each word hashes into a fixed position in a 64-dimensional vector,
/// giving semantically similar texts overlapping non-zero dimensions.
struct MockEmbeddingProvider;

#[async_trait]
impl EmbeddingProvider for MockEmbeddingProvider {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| text_to_vector(t, 64)).collect())
    }

    fn dimensions(&self) -> usize {
        64
    }
}

/// Simple deterministic hash-based vectorization for demo purposes.
fn text_to_vector(text: &str, dims: usize) -> Vec<f32> {
    let mut vec = vec![0.0f32; dims];
    for word in text.split_whitespace() {
        let hash = word.bytes().fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
        let idx = (hash as usize) % dims;
        vec[idx] += 1.0;
    }
    // Normalize to unit vector for cosine similarity
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in &mut vec {
            *v /= norm;
        }
    }
    vec
}

fn make_entry(role: &str, text: &str, author: &str) -> MemoryEntry {
    MemoryEntry {
        content: Content {
            role: role.to_string(),
            parts: vec![Part::Text { text: text.to_string() }],
        },
        author: author.to_string(),
        timestamp: Utc::now(),
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    println!("=== ADK PostgreSQL Memory Service Example ===\n");

    // --- Part 1: Vector similarity search with EmbeddingProvider ---

    println!("--- Part 1: Vector Similarity Search ---\n");

    println!("1. Connecting with mock embedding provider (64 dims)...");
    let provider = std::sync::Arc::new(MockEmbeddingProvider);
    let service = PostgresMemoryService::new(DATABASE_URL, Some(provider)).await?;
    println!("   Connected.\n");

    println!("2. Running migrations (pgvector extension + tables)...");
    service.migrate().await?;
    println!("   Done.\n");

    println!("3. Adding conversation memories...");
    let entries = vec![
        make_entry("user", "How do I configure PostgreSQL connection pooling?", "alice"),
        make_entry(
            "model",
            "Use sqlx PgPool with max_connections and idle_timeout settings.",
            "assistant",
        ),
        make_entry("user", "What about Redis caching strategies?", "alice"),
        make_entry(
            "model",
            "Consider write-through or write-behind caching with TTL expiry.",
            "assistant",
        ),
        make_entry("user", "Explain vector similarity search with pgvector.", "alice"),
        make_entry(
            "model",
            "pgvector adds vector columns and cosine/L2 distance operators to PostgreSQL.",
            "assistant",
        ),
        make_entry("user", "How do I set up authentication with JWT tokens?", "alice"),
        make_entry(
            "model",
            "Use a JWT library to validate tokens and extract claims for authorization.",
            "assistant",
        ),
    ];
    service.add_session("demo-app", "alice", "session-001", entries).await?;
    println!("   Added 8 memory entries.\n");

    println!("4. Searching for 'database connection' (vector similarity)...");
    let results = service
        .search(SearchRequest {
            query: "database connection".to_string(),
            user_id: "alice".to_string(),
            app_name: "demo-app".to_string(),
            limit: None,
            min_score: None,
        })
        .await?;
    println!("   Found {} results:", results.memories.len());
    for (i, entry) in results.memories.iter().enumerate() {
        let text = extract_text(&entry.content);
        println!("   {}. [{}] {}", i + 1, entry.author, truncate(&text, 80));
    }
    println!();

    println!("5. Searching for 'caching TTL' (vector similarity)...");
    let results = service
        .search(SearchRequest {
            query: "caching TTL".to_string(),
            user_id: "alice".to_string(),
            app_name: "demo-app".to_string(),
            limit: None,
            min_score: None,
        })
        .await?;
    println!("   Found {} results:", results.memories.len());
    for (i, entry) in results.memories.iter().enumerate() {
        let text = extract_text(&entry.content);
        println!("   {}. [{}] {}", i + 1, entry.author, truncate(&text, 80));
    }
    println!();

    // --- Part 2: Keyword fallback search (no embedding provider) ---

    println!("--- Part 2: Keyword Fallback Search (no embedding provider) ---\n");

    println!("6. Connecting without embedding provider...");
    let keyword_service = PostgresMemoryService::new(DATABASE_URL, None).await?;
    println!("   Connected.\n");

    println!("7. Adding entries for keyword search...");
    let keyword_entries = vec![
        make_entry("user", "Rust async runtime comparison tokio vs async-std", "bob"),
        make_entry(
            "model",
            "Tokio is the most widely used async runtime in the Rust ecosystem.",
            "assistant",
        ),
        make_entry("user", "How does error handling work with thiserror?", "bob"),
        make_entry(
            "model",
            "thiserror provides derive macros for implementing std Error trait.",
            "assistant",
        ),
    ];
    keyword_service.add_session("demo-app", "bob", "session-002", keyword_entries).await?;
    println!("   Added 4 entries.\n");

    println!("8. Keyword search for 'tokio runtime'...");
    let results = keyword_service
        .search(SearchRequest {
            query: "tokio runtime".to_string(),
            user_id: "bob".to_string(),
            app_name: "demo-app".to_string(),
            limit: None,
            min_score: None,
        })
        .await?;
    println!("   Found {} results:", results.memories.len());
    for (i, entry) in results.memories.iter().enumerate() {
        let text = extract_text(&entry.content);
        println!("   {}. [{}] {}", i + 1, entry.author, truncate(&text, 80));
    }
    println!();

    println!("9. Keyword search for 'error handling'...");
    let results = keyword_service
        .search(SearchRequest {
            query: "error handling".to_string(),
            user_id: "bob".to_string(),
            app_name: "demo-app".to_string(),
            limit: None,
            min_score: None,
        })
        .await?;
    println!("   Found {} results:", results.memories.len());
    for (i, entry) in results.memories.iter().enumerate() {
        let text = extract_text(&entry.content);
        println!("   {}. [{}] {}", i + 1, entry.author, truncate(&text, 80));
    }

    println!("\n=== Memory example completed successfully ===");
    Ok(())
}

fn extract_text(content: &Content) -> String {
    content
        .parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}...", &s[..max]) }
}
