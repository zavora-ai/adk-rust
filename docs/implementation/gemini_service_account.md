# Using GeminiModel with Service Account Authentication

This guide explains how to use GeminiModel with Google Cloud service account authentication instead of API keys.

## Overview

The `GeminiModel` now supports authentication using Google Cloud service accounts. This is useful when:
- Running in Google Cloud environments (GCE, GKE, Cloud Run, etc.)
- You need more fine-grained access control
- You want to use Google Cloud IAM for managing permissions
- API keys are not suitable for your security requirements

## Prerequisites

1. A Google Cloud service account with appropriate permissions
2. The service account JSON key file
3. The `gemini` feature enabled in your `Cargo.toml`:

```toml
[dependencies]
adk-model = { version = "0.2.1", features = ["gemini"] }
```

## Usage

Service account authentication requires using Vertex AI endpoints.

### Using the Builder Pattern (Recommended)

```rust
use adk_model::gemini::{GeminiModel, RetryConfig};

// From service account file
let model = GeminiModel::builder("gemini-2.5-flash")
    .service_account_path("/path/to/service-account.json")?
    .project_id("my-project-id")
    .location("us-central1")
    .retry_config(
        RetryConfig::new()
            .with_max_retries(10)
            .with_initial_delay(Duration::from_millis(500))
    )
    .build()
    .await?;

// From service account JSON string
let model = GeminiModel::builder("gemini-2.5-flash")
    .service_account_json(json_string)
    .project_id("my-project-id")
    .location("us-central1")
    .build()
    .await?;
```

### Convenience Methods

For simpler use cases, you can use the convenience methods:

#### Method 1: From Service Account JSON File

```rust
use adk_model::gemini::GeminiModel;

let model = GeminiModel::new_with_service_account_path(
    "/path/to/service-account.json",
    "my-project-id",
    "us-central1",
    "gemini-2.5-flash"
).await?;
```

#### Method 2: From Service Account JSON String

```rust
let service_account_json = std::fs::read_to_string("service-account.json")?;
let model = GeminiModel::new_with_service_account_json(
    service_account_json,
    "my-project-id",
    "us-central1",
    "gemini-2.5-flash"
).await?;
```

## Example

See the complete example at `examples/gemini_service_account.rs`:

```bash
# Required: Set your GCP project ID
export GOOGLE_PROJECT_ID=my-project-id

# Optional: Set location (defaults to us-central1)
export GOOGLE_LOCATION=us-central1

# Option 1: Set the service account file path
export GOOGLE_SERVICE_ACCOUNT_PATH=/path/to/service-account.json

# Option 2: Or provide the JSON content directly
export GOOGLE_SERVICE_ACCOUNT_JSON='{"type":"service_account",...}'

# Run the example
cargo run --example gemini_service_account --features gemini
```

## How It Works

Under the hood, the implementation:
1. Uses the `gcp_auth` crate to parse the service account JSON and obtain OAuth tokens
2. Creates an authenticated HTTP client with Bearer token authorization
3. Configures the Gemini client to use Vertex AI endpoints (`{location}-aiplatform.googleapis.com`)
4. Automatically handles token refresh through `gcp_auth`

Service account authentication requires Vertex AI endpoints because the consumer Gemini API (`generativelanguage.googleapis.com`) is primarily designed for API key authentication.

## Service Account Permissions

Your service account needs the following permissions:
- For Gemini API: `roles/aiplatform.user` or more specific permissions
- The implementation requests these OAuth scopes:
  - `https://www.googleapis.com/auth/generative-language` - Base scope for generative language API
  - `https://www.googleapis.com/auth/generative-language.retriever` - For semantic retrieval features
  - `https://www.googleapis.com/auth/cloud-platform` - General Google Cloud Platform access

## Security Best Practices

1. **Never commit service account files to version control**
2. Store service account JSON in secure secret management systems
3. Use environment variables or secure file storage for production
4. Rotate service account keys regularly
5. Use the principle of least privilege when assigning permissions

## Configuring Retry Logic

The GeminiModel includes automatic retry logic for handling rate limits (429 errors). You can customize this behavior:

### Default Configuration
By default, retries are enabled with:
- 3 maximum retries
- 1 second initial delay
- 2x exponential backoff
- 60 seconds maximum delay

### Custom Configuration

```rust
use adk_model::gemini::{GeminiModel, RetryConfig};
use std::time::Duration;

// Using the builder pattern (recommended)
let model = GeminiModel::builder("models/gemini-2.5-flash")
    .api_key("your-api-key")
    .retry_config(
        RetryConfig::new()
            .with_max_retries(5)
            .with_initial_delay(Duration::from_millis(500))
            .with_max_delay(Duration::from_secs(30))
            .with_backoff_multiplier(1.5)
    )
    .build()
    .await?;

// Configure with closure
let model = GeminiModel::builder("models/gemini-2.5-flash")
    .api_key("your-api-key")
    .configure_retries(|config| {
        config
            .with_max_retries(10)
            .with_initial_delay(Duration::from_secs(2))
    })
    .build()
    .await?;

// Disable retries
let model = GeminiModel::builder("models/gemini-2.5-flash")
    .api_key("your-api-key")
    .retry_config(RetryConfig::disabled())
    .build()
    .await?;
```

### With Service Accounts

```rust
let model = GeminiModel::builder("gemini-2.5-flash")
    .service_account_path("service-account.json")?
    .project_id("project-id")
    .location("us-central1")
    .retry_config(
        RetryConfig::new()
            .with_max_retries(10)  // More retries for production
            .with_backoff_multiplier(3.0)  // More aggressive backoff
    )
    .build()
    .await?;
```

## Troubleshooting

### "Failed to parse service account JSON"
- Ensure the JSON is valid and contains all required fields
- Check that the file path is correct and the file is readable

### "Failed to get access token"
- Verify the service account has the necessary permissions
- Check that the service account is not disabled
- Ensure you have network connectivity to Google's OAuth2 endpoints

### "404 Not Found" error
- Verify your project ID is correct
- Ensure the location/region is valid (e.g., "us-central1")
- Check that the Vertex AI API is enabled in your GCP project
- Verify the model name is correct (e.g., "gemini-2.5-flash")

### "403 Forbidden" error
- Ensure the service account has the `roles/aiplatform.user` role or equivalent permissions
- Verify the service account belongs to the correct project

### Token Expiration
- Tokens are automatically refreshed by the `gcp_auth` crate
- No manual token management is required