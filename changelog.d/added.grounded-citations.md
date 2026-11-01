- **Grounded citations and `CitationRequired`** (`adk-core`, `adk-guardrail`, `adk-agent`,
  `adk-rag`): nothing tied an answer's citations to what the agent retrieved, so a guardrail could
  only search the text for a link. A retrieval tool records each source it gives the model as an
  `adk_core::GroundingSource` on the new `EventActions::grounding_sources`, with
  `record_grounding_sources`; `RagTool` and `VertexAiRagRetrievalTool` do. `LlmAgent` and `CodeActAgent` bind
  the final answer's markdown links to a source their run recorded into
  `LlmResponse::citation_metadata` (`adk_core::bind_markdown_citations`) and screen the answer
  through the new `Guardrail::validate_output`, which receives the sources and citations as
  `OutputEvidence` and defaults to `validate`. `CitationRequired` blocks an answer that cites
  fewer than `min_citations` retrieved sources, or that was given while nothing was retrieved
  unless `WhenNothingRetrieved::Pass` is set; provider citations of other URIs count only with
  `trust_provider_citations`. `GuardrailExecutor::enforce_output` runs the screening. Refs #1279.
