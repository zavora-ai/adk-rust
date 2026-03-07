//! Core types doc-test - validates core.md documentation

use adk_core::{Content, Part};

fn main() {
    println!("=== Core Types Doc-Test ===\n");

    // From docs: Create text content
    let user_msg = Content::user().with_text("Hello!");
    assert_eq!(user_msg.role.to_string(), "user");
    assert_eq!(user_msg.parts.len(), 1);
    println!("✓ Content::user().with_text() works");

    let model_msg = Content::model().with_text("Hi there!");
    assert_eq!(model_msg.role.to_string(), "model");
    println!("✓ Model content works");

    // From docs: Create multimodal content
    let image_bytes = vec![0x89, 0x50, 0x4E, 0x47]; // PNG header
    let content = Content::user()
        .with_text("What's in this image?")
        .with_inline_data("image/png", image_bytes)
        .unwrap();
    assert_eq!(content.parts.len(), 2);
    println!("✓ Multimodal content with inline data works");

    // From docs: Create content with file URI
    let content = Content::user()
        .with_text("Here's a document:")
        .with_file_uri("application/pdf", "https://example.com/doc.pdf");
    assert_eq!(content.parts.len(), 2);
    println!("✓ Content with file URI works");

    // From docs: Part helpers
    let text = Part::text("Hello");
    assert!(matches!(text, Part::Text(..)));
    println!("✓ Part::text() works");

    let image = Part::inline_data("image/png", vec![1, 2, 3]).unwrap();
    assert!(matches!(image, Part::InlineData { .. }));
    println!("✓ Part::inline_data() works");

    let file = Part::file_data("image/jpeg", "https://example.com/img.jpg");
    assert!(matches!(file, Part::FileData { .. }));
    println!("✓ Part::file_data() works");

    // From docs: Access part data
    let part = Part::text("Hello world");
    assert_eq!(part.as_text_str(), Some("Hello world"));
    println!("✓ part.as_text_str() works");

    let part = Part::inline_data("image/png", vec![]).unwrap();
    assert_eq!(part.mime_type(), Some("image/png"));
    println!("✓ part.mime_type() works");

    let part = Part::file_data("image/jpeg", "https://example.com/img.jpg");
    assert_eq!(part.file_uri(), Some("https://example.com/img.jpg"));
    println!("✓ part.file_uri() works");

    // From docs: is_media check
    let text_part = Part::text("text");
    assert!(!text_part.is_media());

    let media_part = Part::inline_data("image/png", vec![]).unwrap();
    assert!(media_part.is_media());
    println!("✓ part.is_media() works");

    println!("\n=== All core types tests passed! ===");
}
