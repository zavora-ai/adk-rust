//! RAG doc-test — validates chunker examples from rag.md

use adk_rag::chunking::Chunker;
use adk_rag::{Document, FixedSizeChunker, MarkdownChunker, RecursiveChunker};

fn sample_doc() -> Document {
    Document {
        id: "test".to_string(),
        text: "First paragraph about Rust programming.\n\n\
               Second paragraph about memory safety. Rust prevents dangling pointers.\n\n\
               Third paragraph about async. Tokio is the most popular runtime."
            .to_string(),
        metadata: Default::default(),
        source_uri: None,
    }
}

fn markdown_doc() -> Document {
    Document {
        id: "md".to_string(),
        text: "# Getting Started\n\n\
               Install the CLI tool.\n\n\
               ## Installation\n\n\
               Run npm install to get started.\n\n\
               ## Configuration\n\n\
               Edit the config file."
            .to_string(),
        metadata: Default::default(),
        source_uri: None,
    }
}

fn main() {
    println!("=== RAG Chunkers Doc-Test ===\n");

    // From docs: FixedSizeChunker
    let chunker = FixedSizeChunker::new(512, 100);
    let chunks = chunker.chunk(&sample_doc());
    assert!(!chunks.is_empty());
    for chunk in &chunks {
        assert!(!chunk.text.is_empty());
        assert!(chunk.text.len() <= 512);
        assert_eq!(chunk.document_id, "test");
    }
    println!("✓ FixedSizeChunker works — {} chunks", chunks.len());

    // From docs: RecursiveChunker
    let chunker = RecursiveChunker::new(512, 100);
    let chunks = chunker.chunk(&sample_doc());
    assert!(!chunks.is_empty());
    for chunk in &chunks {
        assert!(!chunk.text.is_empty());
    }
    println!("✓ RecursiveChunker works — {} chunks", chunks.len());

    // From docs: MarkdownChunker
    let chunker = MarkdownChunker::new(512, 100);
    let chunks = chunker.chunk(&markdown_doc());
    assert!(!chunks.is_empty());

    // Verify header_path metadata is preserved
    let has_header_path = chunks.iter().any(|c| c.metadata.contains_key("header_path"));
    assert!(has_header_path, "MarkdownChunker should set header_path metadata");
    println!("✓ MarkdownChunker works — {} chunks with header_path", chunks.len());

    // Verify chunk IDs follow document_id_index pattern
    for (i, chunk) in chunks.iter().enumerate() {
        assert_eq!(chunk.id, format!("md_{i}"));
    }
    println!("✓ Chunk IDs follow expected pattern");

    // Empty document produces no chunks
    let empty = Document {
        id: "empty".to_string(),
        text: String::new(),
        metadata: Default::default(),
        source_uri: None,
    };
    assert!(FixedSizeChunker::new(100, 10).chunk(&empty).is_empty());
    assert!(RecursiveChunker::new(100, 10).chunk(&empty).is_empty());
    assert!(MarkdownChunker::new(100, 10).chunk(&empty).is_empty());
    println!("✓ Empty documents produce no chunks");

    println!("\n=== All chunker tests passed! ===");
}
