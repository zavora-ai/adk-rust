//! Live integration test for the Vertex AI Agent Engine sandbox client.
//!
//! Requires a provisioned Agent Engine and ADC:
//!
//! ```bash
//! export GOOGLE_CLOUD_PROJECT=my-project
//! export GOOGLE_CLOUD_LOCATION=us-central1
//! export GOOGLE_CLOUD_AGENT_ENGINE_ID=1234567890
//! cargo nextest run -p adk-code --features vertex-sandbox \
//!     --run-ignored all -E 'test(live_sandbox_round_trip)'
//! ```

#![cfg(feature = "vertex-sandbox")]

use adk_code::vertex_sandbox::{
    CreateSandboxRequest, InputFile, VertexSandboxClient, VertexSandboxConfig,
};

#[tokio::test]
#[ignore = "requires ADC and a provisioned Agent Engine"]
async fn live_sandbox_round_trip() {
    let config = VertexSandboxConfig::from_env().expect("platform env vars set");
    let engine = std::env::var("GOOGLE_CLOUD_AGENT_ENGINE_ID")
        .expect("GOOGLE_CLOUD_AGENT_ENGINE_ID names the reasoning engine");
    let client = VertexSandboxClient::new_with_adc(config).expect("ADC available");

    let sandbox = client
        .create_sandbox(&engine, CreateSandboxRequest::new("adk-rust-live-test").with_ttl("600s"))
        .await
        .expect("sandbox create succeeds");
    let name = sandbox.name.expect("created sandbox has a name");

    let files = [InputFile::new("data.txt", "text/plain", b"hello from adk-rust".to_vec())];
    let result = client
        .execute_code(
            &name,
            "print(open('data.txt').read())\nopen('copy.txt', 'w').write(open('data.txt').read())",
            &files,
        )
        .await
        .expect(":execute succeeds");
    assert!(
        result.stdout.contains("hello from adk-rust"),
        "stdout did not carry the file contents: {result:?}",
    );

    let listed = client.list_sandboxes(&engine).await.expect("list succeeds");
    assert!(
        listed.iter().any(|sandbox| sandbox.name.as_deref() == Some(name.as_str())),
        "created sandbox missing from list",
    );

    client.delete_sandbox(&name).await.expect("sandbox delete succeeds");
}
