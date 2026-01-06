//! Core types doc-test - validates core.md documentation

use adk_core::{Content, Part};

fn main() {
    println!("=== Core Types Doc-Test ===\n");

    // From docs: Create text content
    let user_msg = Content::new("user").with_text("Hello!");
    assert_eq!(user_msg.role, "user");
    assert_eq!(user_msg.parts.len(), 1);
    println!("✓ Content::new().with_text() works");

    let model_msg = Content::new("model").with_text("Hi there!");
    assert_eq!(model_msg.role, "model");
    println!("✓ Model content works");

    // From docs: Create multimodal content
    let image_bytes = vec![0x89, 0x50, 0x4E, 0x47]; // PNG header
    let content = Content::new("user")
        .with_text("What's in this image?")
        .with_inline_data("image/png", image_bytes);
    assert_eq!(content.parts.len(), 2);
    println!("✓ Multimodal content with inline data works");

    // From docs: Create content with file URI
    let content = Content::new("user")
        .with_text("Analyze this document")
        .with_file_uri("application/pdf", "https://example.com/doc.pdf");
    assert_eq!(content.parts.len(), 2);
    println!("✓ Content with file URI works");

    // From docs: Part helpers
    let text = Part::text_part("Hello");
    assert!(matches!(text, Part::Text { .. }));
    println!("✓ Part::text_part() works");

    let image = Part::inline_data("image/png", vec![1, 2, 3]);
    assert!(matches!(image, Part::InlineData { .. }));
    println!("✓ Part::inline_data() works");

    let file = Part::file_data("image/jpeg", "https://example.com/img.jpg");
    assert!(matches!(file, Part::FileData { .. }));
    println!("✓ Part::file_data() works");

    // From docs: Access part data
    let part = Part::text_part("Hello world");
    assert_eq!(part.text(), Some("Hello world"));
    println!("✓ part.text() works");

    let part = Part::inline_data("image/png", vec![]);
    assert_eq!(part.mime_type(), Some("image/png"));
    println!("✓ part.mime_type() works");

    let part = Part::file_data("image/jpeg", "https://example.com/img.jpg");
    assert_eq!(part.file_uri(), Some("https://example.com/img.jpg"));
    println!("✓ part.file_uri() works");

    // From docs: is_media check
    let text_part = Part::text_part("text");
    assert!(!text_part.is_media());

    let media_part = Part::inline_data("image/png", vec![]);
    assert!(media_part.is_media());
    println!("✓ part.is_media() works");

    println!("\n=== All core types tests passed! ===");
}
