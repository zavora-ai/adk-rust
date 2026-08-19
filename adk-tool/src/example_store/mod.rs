//! Vertex AI Example Store client and few-shot retrieval provider.
//!
//! The [Example Store](https://cloud.google.com/vertex-ai/generative-ai/docs/example-store/overview)
//! is a managed Vertex AI service that stores few-shot examples and retrieves
//! the ones most relevant to an incoming request. This module provides:
//!
//! - [`ExampleStoreClient`] — an ADC-authenticated REST client for the
//!   `v1beta1` Example Store **data plane**: [`upsert_examples`],
//!   [`search_examples`], and [`fetch_examples`] against a pre-provisioned
//!   `projects/*/locations/*/exampleStores/*` resource. Store creation and
//!   deletion are control-plane provisioning concerns and are out of scope.
//! - [`ExampleStoreProvider`] — a helper that retrieves top-k similar examples
//!   for the incoming user message and injects them into the request preamble,
//!   packaged as a [`BeforeModelCallback`](adk_core::BeforeModelCallback).
//!
//! > **Note:** the Example Store API is **v1beta1 (Preview)** and is currently
//! > served from the `us-central1` region only.
//!
//! # Example
//!
//! ```no_run
//! use adk_tool::example_store::{
//!     ContentsExample, Example, ExampleStoreClient, ExampleStoreConfig, SearchExamplesRequest,
//!     StoredContentsExample, UpsertExamplesRequest,
//! };
//! use adk_core::Content;
//!
//! # async fn demo() -> adk_core::Result<()> {
//! let config = ExampleStoreConfig::new("my-project", "us-central1", "my-store");
//! let client = ExampleStoreClient::new_with_adc(config)?;
//!
//! let example = Example::new(
//!     StoredContentsExample::new(ContentsExample::new(
//!         vec![Content::new("user").with_text("What is the capital of France?")],
//!         vec![Content::new("model").with_text("Paris.")],
//!     ))
//!     .with_search_key("What is the capital of France?"),
//! );
//! client.upsert_examples(UpsertExamplesRequest::new(vec![example])).await?;
//!
//! let results = client
//!     .search_examples(SearchExamplesRequest::by_search_key("capital cities", 5))
//!     .await?;
//! for result in results.results {
//!     println!("score: {:?}", result.similarity_score);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! [`upsert_examples`]: ExampleStoreClient::upsert_examples
//! [`search_examples`]: ExampleStoreClient::search_examples
//! [`fetch_examples`]: ExampleStoreClient::fetch_examples

mod client;
mod provider;

pub use client::{
    ArrayOperator, ContentSearchKey, ContentsExample, Example, ExampleStoreClient,
    ExampleStoreConfig, ExamplesArrayFilter, ExpectedContent, FetchExamplesRequest,
    FetchExamplesResponse, LastEntry, RpcStatus, SearchExampleResult, SearchExamplesParameters,
    SearchExamplesRequest, SearchExamplesResponse, SearchKeyGenerationMethod,
    StoredContentsExample, StoredContentsExampleFilter, StoredContentsExampleParameters,
    UpsertExamplesRequest, UpsertExamplesResponse, UpsertResult,
};
pub use provider::ExampleStoreProvider;
