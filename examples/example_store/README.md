# Example Store — dynamic few-shot retrieval

Demonstrates the Vertex AI Example Store client in `adk-tool` (feature
`example-store`): upserting few-shot examples into a pre-provisioned store,
searching for the ones most similar to a query, and formatting the results the
way `ExampleStoreProvider` injects them into agent requests.

> **Note:** the Example Store API is **v1beta1 (Preview)** and is currently
> served from the `us-central1` region only. The store itself must be
> pre-provisioned — this client performs data-plane operations only.

## Prerequisites

1. A Google Cloud project with the Vertex AI API enabled.
2. A pre-provisioned Example Store in `us-central1` (see the
   [Example Store overview](https://cloud.google.com/vertex-ai/generative-ai/docs/example-store/overview)).
3. Application Default Credentials:

   ```bash
   gcloud auth application-default login
   ```

## Setup

```bash
cp .env.example .env
# then fill in your project, location, and store ID
```

| Variable | Description |
|----------|-------------|
| `GOOGLE_CLOUD_PROJECT` | Google Cloud project ID |
| `GOOGLE_CLOUD_LOCATION` | Region — Example Store supports `us-central1` only |
| `EXAMPLE_STORE_ID` | ID of the pre-provisioned Example Store |

## Run

```bash
cargo run --manifest-path examples/example_store/Cargo.toml
```

The example:

1. Upserts three support-style examples with explicit search keys
   (`overwrite: true` keeps reruns idempotent).
2. Searches for the top-3 examples most similar to
   `"I forgot my login credentials"` and prints their similarity scores.
3. Formats the results as the few-shot instruction block that
   `ExampleStoreProvider::into_before_model_callback()` prepends to agent
   requests.

## Using the provider in an agent

```rust,ignore
use adk_tool::example_store::{ExampleStoreClient, ExampleStoreConfig, ExampleStoreProvider};
use std::sync::Arc;

let client = Arc::new(ExampleStoreClient::new_with_adc(ExampleStoreConfig::from_env()?)?);
let provider = ExampleStoreProvider::new(client).with_top_k(3);

let agent = LlmAgentBuilder::new("support")
    .model(model)
    .instruction("You are a helpful support assistant.")
    .before_model_callback(provider.into_before_model_callback())
    .build()?;
```

On every model call the callback searches the store with the latest user
message and prepends the retrieved examples to the request preamble — the same
position the system instruction occupies.
