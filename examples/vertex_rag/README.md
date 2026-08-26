# Vertex AI RAG Engine — agentic corpus retrieval

Runs an `LlmAgent` that grounds its answers in a Vertex AI RAG Engine corpus
via `VertexAiRagRetrievalTool` (`adk-rag`, feature `vertex-rag`).

## What this shows

- `VertexRagConfig::from_env()` + `VertexRagEngineClient::new_with_adc()` —
  ADC-authenticated, read-only RAG Engine client
- `ensure_corpus_ready()` — fails fast with actionable guidance when the
  corpus is missing, empty, or in the `ERROR` state
- `VertexAiRagRetrievalTool` — retrieval as an `adk_core::Tool`; the agent
  calls `vertex_rag_retrieval` with a query and receives a JSON array of
  `{text, sourceUri, sourceDisplayName, score}` passages
- `similarity_top_k` / `vector_distance_threshold` — retrieval knobs sent on
  the current `query.ragRetrievalConfig` wire path

## Prerequisites

1. A pre-provisioned RAG corpus with imported files — corpus creation and
   ingestion are out of scope for this client. Create one in the
   [Vertex AI console](https://console.cloud.google.com/vertex-ai/rag) or with
   the `RagCorpora`/`RagFiles` management APIs.
2. Application Default Credentials:

   ```bash
   gcloud auth application-default login
   ```

3. A Gemini API key for the LLM provider
   ([Google AI Studio](https://aistudio.google.com/apikey)).

## Run

```bash
cp examples/vertex_rag/.env.example examples/vertex_rag/.env
# edit examples/vertex_rag/.env

cargo run --manifest-path examples/vertex_rag/Cargo.toml
```

## Environment variables

| Variable | Description |
|----------|-------------|
| `GOOGLE_API_KEY` | Gemini API key for the agent's LLM |
| `GOOGLE_CLOUD_PROJECT` | Project that owns the corpus |
| `GOOGLE_CLOUD_LOCATION` | Region the corpus lives in |
| `VERTEX_RAG_CORPUS` | Corpus ID or full `projects/*/locations/*/ragCorpora/*` name |
| `RAG_QUESTION` | Optional: overrides the default question |
