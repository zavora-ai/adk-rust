- **Long-term facts with provenance** (`adk-harness`, spec 3.1 §7.15.2): the harness learns
  short facts — preferences, project facts, procedures, outcomes, decisions, corrections —
  and recalls them into a delimited `<memory-facts>` block on the first model call of each
  user turn, `known` before `assumed`, within `recall_tokens`.
  - `MemoryFact` carries `FactStatus` (`Known`, `Assumed`, `Superseded`, `Forgotten`) and
    `Provenance` (source, session, invocation, verified quote). A fact is `Known` only from
    a quote found verbatim in one of the user's own messages, or a person's confirmation;
    model output, tool output and files produce `Assumed` facts at most.
  - `FactStore` is the system of record (`InMemoryFactStore`, `FileFactStore`,
    `SqliteFactStore` with `sqlite`); `FactIndex` finds candidates (`LocalFactIndex` by
    cosine similarity or shared words, `MemoryServiceIndex` over any `adk-memory` backend).
    `fact_store_conformance_tests!` and `fact_index_conformance_tests!` check backends.
  - Writing: `memory_remember` (assumed only, class `MemoryPropose`: denied in ReadOnly and
    Plan, allowed in Ask and Auto), session distillation on the member model with no tools
    and structured output under a `RunBudget`, and `Harness::{accept_fact, forget_fact,
    correct_fact}`; `memory_search` reads on demand. Text is redacted with `PiiRedactor`
    and a fact holding a secret is refused. Team members neither recall nor write.
  - `HarnessAgentBuilder::{long_term_memory, fact_store, fact_index, memory_embedder}`;
    off until `long_term_memory` is called. Feature `memory-embeddings` adds `RagEmbedder`
    over `adk-rag`'s OpenAI and Gemini embedders.
  - Audit records `harness.memory.fact.stored`, `.promoted`, `.superseded`, `.forgotten`,
    `harness.memory.recalled` and `harness.memory.distilled` carry ids and digests, never
    text.
- **`MemoryService` conformance for project scope** (`adk-memory`, spec 3.1 §10 M2): every
  backend round-trips `author` and honours `project_id` in `add_entry_to_project`, `search`
  and `delete_entries_in_project`, on in-memory, SQLite and PostgreSQL.
- **`long_term_memory_across_sessions` scenario** (`harness_agent` example): H11 scripted
  against `HarnessAgent` and the 3.0 `CodingAgent`.
