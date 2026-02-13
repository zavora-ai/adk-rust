#[cfg(feature = "vertex")]
use adk_gemini::GeminiLiveBackend;

#[tokio::main]
async fn main() {
    println!("Verifying Vertex AI support...");

    #[cfg(feature = "vertex")]
    {
        // Explain how authentication works for VertexADC
        println!(
            "This example demonstrates the configuration for Vertex AI using Application Default Credentials (ADC)."
        );
        println!(
            "Authentication is handled automatically by the Google Cloud libraries using the environment:"
        );
        println!("1. Checks for GOOGLE_APPLICATION_CREDENTIALS environment variable.");
        println!(
            "2. Checks for user credentials set up via 'gcloud auth application-default login'."
        );
        println!("3. Checks for metadata server (if running on GCP).");
        println!();

        // Attempt to instantiate the VertexADC variant to prove it's available
        let backend = GeminiLiveBackend::VertexADC {
            project: "test-project".to_string(),
            location: "us-central1".to_string(),
        };

        println!("Successfully instantiated VertexADC backend configuration: {:?}", backend);
        println!("Note: No API key is required in the code itself.");
    }

    #[cfg(not(feature = "vertex"))]
    {
        println!("Vertex feature is not enabled. Please run with --features vertex");
    }
}
