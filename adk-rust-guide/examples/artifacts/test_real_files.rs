use adk_rust::prelude::*;
use adk_rust::artifact::{ArtifactService, InMemoryArtifactService, SaveRequest, LoadRequest, ListRequest};
use adk_rust_guide::{print_success, print_validating};

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    print_validating("artifacts/artifacts.md - Real File Testing");

    // Create an in-memory artifact service
    let artifact_service = InMemoryArtifactService::new();

    println!("=== Testing Real File Artifacts ===\n");

    // Test CSV file
    let csv_content = std::fs::read_to_string("examples/artifacts/test_data.csv")?;
    let csv_part = Part::InlineData {
        data: csv_content.as_bytes().to_vec(),
        mime_type: "text/csv".to_string(),
    };
    
    let csv_resp = artifact_service.save(SaveRequest {
        app_name: "test_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: "session_1".to_string(),
        file_name: "employee_data.csv".to_string(),
        part: csv_part,
        version: None,
    }).await?;
    println!("✓ Saved CSV file: version {}, {} bytes", csv_resp.version, csv_content.len());

    // Test TXT file
    let txt_content = std::fs::read_to_string("examples/artifacts/test_document.txt")?;
    let txt_part = Part::Text { text: txt_content.clone() };
    
    let txt_resp = artifact_service.save(SaveRequest {
        app_name: "test_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: "session_1".to_string(),
        file_name: "documentation.txt".to_string(),
        part: txt_part,
        version: None,
    }).await?;
    println!("✓ Saved TXT file: version {}, {} bytes", txt_resp.version, txt_content.len());

    // Test JSON file
    let json_content = std::fs::read_to_string("examples/artifacts/test_config.json")?;
    let json_part = Part::InlineData {
        data: json_content.as_bytes().to_vec(),
        mime_type: "application/json".to_string(),
    };
    
    let json_resp = artifact_service.save(SaveRequest {
        app_name: "test_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: "session_1".to_string(),
        file_name: "config.json".to_string(),
        part: json_part,
        version: None,
    }).await?;
    println!("✓ Saved JSON file: version {}, {} bytes", json_resp.version, json_content.len());

    // Test binary file (PNG)
    let png_content = std::fs::read("examples/artifacts/test_image.png")?;
    let png_part = Part::InlineData {
        data: png_content.clone(),
        mime_type: "image/png".to_string(),
    };
    
    let png_resp = artifact_service.save(SaveRequest {
        app_name: "test_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: "session_1".to_string(),
        file_name: "test_image.png".to_string(),
        part: png_part,
        version: None,
    }).await?;
    println!("✓ Saved PNG file: version {}, {} bytes", png_resp.version, png_content.len());

    // List all artifacts
    let list_resp = artifact_service.list(ListRequest {
        app_name: "test_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: "session_1".to_string(),
    }).await?;
    println!("\nStored artifacts: {:?}", list_resp.file_names);

    // Test loading artifacts
    println!("\n=== Testing Artifact Loading ===");
    
    let csv_loaded = artifact_service.load(LoadRequest {
        app_name: "test_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: "session_1".to_string(),
        file_name: "employee_data.csv".to_string(),
        version: None,
    }).await?;
    
    let txt_loaded = artifact_service.load(LoadRequest {
        app_name: "test_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: "session_1".to_string(),
        file_name: "documentation.txt".to_string(),
        version: None,
    }).await?;
    
    let json_loaded = artifact_service.load(LoadRequest {
        app_name: "test_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: "session_1".to_string(),
        file_name: "config.json".to_string(),
        version: None,
    }).await?;

    let png_loaded = artifact_service.load(LoadRequest {
        app_name: "test_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: "session_1".to_string(),
        file_name: "test_image.png".to_string(),
        version: None,
    }).await?;

    // Verify content integrity
    match csv_loaded.part {
        Part::InlineData { data, mime_type } => {
            println!("✓ Loaded CSV: {} bytes, MIME: {}", data.len(), mime_type);
            assert_eq!(String::from_utf8(data).unwrap(), csv_content);
        }
        _ => panic!("Expected InlineData for CSV"),
    }

    match txt_loaded.part {
        Part::Text { text } => {
            println!("✓ Loaded TXT: {} bytes", text.len());
            assert_eq!(text, txt_content);
        }
        _ => panic!("Expected Text for TXT"),
    }

    match json_loaded.part {
        Part::InlineData { data, mime_type } => {
            println!("✓ Loaded JSON: {} bytes, MIME: {}", data.len(), mime_type);
            assert_eq!(String::from_utf8(data).unwrap(), json_content);
        }
        _ => panic!("Expected InlineData for JSON"),
    }

    match png_loaded.part {
        Part::InlineData { data, mime_type } => {
            println!("✓ Loaded PNG: {} bytes, MIME: {}", data.len(), mime_type);
            assert_eq!(data, png_content);
        }
        _ => panic!("Expected InlineData for PNG"),
    }

    println!("✓ Content integrity verified for all file types");

    print_success("artifact_real_files");
    
    println!("\nArtifact Service successfully tested with:");
    println!("  - CSV files (text/csv)");
    println!("  - TXT files (Part::Text)");
    println!("  - JSON files (application/json)");
    println!("  - PNG files (image/png) - binary data");

    Ok(())
}
