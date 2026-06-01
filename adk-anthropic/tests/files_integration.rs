//! Integration tests for the Anthropic Files API.
//!
//! These tests require a real `ANTHROPIC_API_KEY` environment variable and are
//! marked `#[ignore]` so they don't run in CI by default.
//!
//! Run with:
//! ```bash
//! cargo test -p adk-anthropic --features files --test files_integration -- --ignored
//! ```

#![cfg(feature = "files")]

use adk_anthropic::files::FilesClient;

fn test_client() -> Option<FilesClient> {
    match FilesClient::from_env() {
        Ok(client) => Some(client),
        Err(_) => {
            eprintln!("ANTHROPIC_API_KEY not set, skipping integration test");
            None
        }
    }
}

#[tokio::test]
#[ignore]
async fn test_upload_and_get_text_file() {
    let Some(client) = test_client() else { return };

    let content = b"Hello, this is a test file for the ADK-Rust Files API integration.".to_vec();
    let file = client
        .upload_file("test_upload.txt", content.clone())
        .await
        .expect("failed to upload file");

    assert!(!file.id.is_empty());
    assert_eq!(file.filename.as_deref(), Some("test_upload.txt"));
    assert_eq!(file.size_bytes, content.len() as u64);
    eprintln!("Uploaded file: {} ({} bytes)", file.id, file.size_bytes);

    // Get file metadata
    let retrieved = client.get_file(&file.id).await.expect("failed to get file");
    assert_eq!(retrieved.id, file.id);
    assert_eq!(retrieved.filename, file.filename);

    // Clean up
    client.delete_file(&file.id).await.expect("failed to delete file");
}

#[tokio::test]
#[ignore]
async fn test_upload_and_delete_pdf() {
    let Some(client) = test_client() else { return };

    // Create a minimal valid PDF
    let pdf_content = b"%PDF-1.0\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/MediaBox[0 0 612 792]/Parent 2 0 R>>endobj\nxref\n0 4\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n0000000115 00000 n \ntrailer<</Size 4/Root 1 0 R>>\nstartxref\n190\n%%EOF".to_vec();

    let file = client.upload_file("test.pdf", pdf_content).await.expect("failed to upload PDF");

    assert!(!file.id.is_empty());
    assert_eq!(file.filename.as_deref(), Some("test.pdf"));
    eprintln!("Uploaded PDF: {}", file.id);

    // Delete
    client.delete_file(&file.id).await.expect("failed to delete PDF");

    // Verify it's gone
    let result = client.get_file(&file.id).await;
    assert!(result.is_err(), "file should not exist after deletion");
}

#[tokio::test]
#[ignore]
async fn test_list_files() {
    let Some(client) = test_client() else { return };

    // Upload a file so we have at least one
    let file = client
        .upload_file("list_test.txt", b"list test content".to_vec())
        .await
        .expect("failed to upload file");

    // List files
    let files = client.list_files().await.expect("failed to list files");
    assert!(files.iter().any(|f| f.id == file.id), "uploaded file should appear in list");
    eprintln!("Listed {} files", files.len());

    // Clean up
    let _ = client.delete_file(&file.id).await;
}

#[tokio::test]
#[ignore]
async fn test_upload_image() {
    let Some(client) = test_client() else { return };

    // Create a minimal 1x1 PNG
    let png_bytes: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
        0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE, // 8-bit RGB
        0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, // IDAT chunk
        0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xE2, 0x21, 0xBC,
        0x33, // compressed data
        0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, // IEND chunk
        0xAE, 0x42, 0x60, 0x82,
    ];

    let file =
        client.upload_file("test_image.png", png_bytes).await.expect("failed to upload image");

    assert!(!file.id.is_empty());
    assert_eq!(file.filename.as_deref(), Some("test_image.png"));
    eprintln!("Uploaded image: {} (mime: {:?})", file.id, file.mime_type);

    // Clean up
    client.delete_file(&file.id).await.expect("failed to delete image");
}

#[tokio::test]
#[ignore]
async fn test_file_not_found() {
    let Some(client) = test_client() else { return };

    let result = client.get_file("file_nonexistent_id_12345").await;
    assert!(result.is_err(), "should return error for nonexistent file");
}

#[tokio::test]
#[ignore]
async fn test_download_uploaded_file_fails() {
    let Some(client) = test_client() else { return };

    // Upload a file
    let file = client
        .upload_file("no_download.txt", b"cannot download this".to_vec())
        .await
        .expect("failed to upload file");

    // Attempt to download (should fail — only code execution outputs are downloadable)
    let result = client.download_file(&file.id).await;
    // The API may return 403 or 400 for non-downloadable files
    if result.is_err() {
        eprintln!("Expected: download of uploaded file failed: {:?}", result.unwrap_err());
    } else {
        eprintln!("Note: download succeeded (API behavior may have changed)");
    }

    // Clean up
    client.delete_file(&file.id).await.expect("failed to delete file");
}

/// Cleanup utility: delete all test files.
#[tokio::test]
#[ignore]
async fn test_cleanup_test_files() {
    let Some(client) = test_client() else { return };

    let files = client.list_files().await.unwrap_or_default();
    let mut deleted = 0;
    for file in &files {
        let name = file.filename.as_deref().unwrap_or("");
        if name.starts_with("test")
            || name.starts_with("list_test")
            || name.starts_with("no_download")
        {
            if client.delete_file(&file.id).await.is_ok() {
                deleted += 1;
                eprintln!("Deleted file: {} ({})", file.id, name);
            }
        }
    }
    eprintln!("Cleaned up {deleted} test files out of {} total", files.len());
}
