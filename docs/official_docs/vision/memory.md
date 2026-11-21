# Visual Memory

`VisualMemory` keeps images by their embeddings in any `adk-rag` vector store and finds them
again by a text question or by an example image. Each image is one `adk_rag::Chunk` whose
text is its caption, whose embedding is the image vector, and whose metadata links to the
artifact holding the image and the run that stored it. Captions also go to `adk-memory` with
a reference to their artifact, sources and labels go to its graph memory, and erasing a
source removes all of it. This page implements spec 3.7 §7.15
([spec](../../design/spec-3.7-vision.md)).

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["memory", "onnx"] }
adk-rag = "3.0.0"
adk-memory = { version = "3.0.0", features = ["graph-memory"] }
```

`memory` brings the visual memory and the S16 week corpus; `onnx` brings `SigLip2`, the
embedder whose texts and images share one space (spec Q14: SigLIP 2 base by default,
so400m opt-in).

## Indexing

`VisualMemory::builder(runtime, store, collection)` takes a `VisionRuntime` with an embedder,
any `Arc<dyn VectorStore>` and a collection name; `open()` creates the collection if needed
and checks its embedding space ([One space per collection](#one-space-per-collection)).
`index(cx, scope, items)` embeds each `VisualItem` through the runtime — on the device for
SigLIP 2, metered against the run's budget like every vision call — and upserts one chunk per
item into the `adk-rag` scope.

| Chunk field | Holds |
|-------------|-------|
| `id` | `vm-` and a digest of source, image digest and region: indexing an item again replaces its chunk |
| `text` | The caption |
| `embedding` | The image vector, or the region's when the item names one |
| `document_id` | The frame source or document, so a source is one filter away |
| `source_uri` | The artifact's URI, `artifact://<app>/<user>/<session>/<file>#<version>` |
| `metadata` | The keys below |

| Metadata key | Value | Filterable |
|--------------|-------|------------|
| `kind` | `frame`, `page` or `image` | yes |
| `day` | The UTC date of capture, `YYYY-MM-DD` | yes |
| `run` | The run (invocation) that stored the image | yes |
| `space` | The collection's space key, `<model>@<revision>/<dimensions>/<metric>` | yes |
| `digest` | `sha256:<hex>` of the image's bytes | yes |
| `artifact` | The artifact locator, as JSON | — |
| `capturedAt` | RFC 3339 in UTC | — |
| `region` | The embedded region in source pixels, as JSON | — |
| `source`, `mimeType`, `size`, `labels`, `caption` | The source, media type, `<w>x<h>`, labels, and the caption's memory id | — |

The collection declares the five filterable keys when it is created, so backends that store
filterable metadata as columns (LanceDB, Agent Retrieval) can filter on them.

```rust
use std::sync::Arc;

use adk_rag::{InMemoryVectorStore, Scope};
use adk_vision::memory::{VisualItem, VisualMemory};
use adk_vision::{ImageRef, SourceImage, VisionCx, VisionRuntime};

async fn remember(
    runtime: Arc<VisionRuntime>,
    image: ImageRef,
    pixels: SourceImage,
) -> adk_vision::Result<()> {
    let memory = VisualMemory::builder(runtime, Arc::new(InMemoryVectorStore::new()), "inspector")
        .open()
        .await?;
    let item = VisualItem::new(image, pixels, "carton 00117, seal torn", "conveyor-1", chrono::Utc::now())
        .with_labels(["torn seal"])
        .with_run("inspector-run-42");
    let scope = Scope::from_parts(["inspector", "plant-7"]).expect("a scope");
    memory.index(&VisionCx::current(), &scope, &[item]).await?;
    Ok(())
}
```

An item's pixels must be the image its `ImageRef` names; `index` refuses a digest mismatch.
Whole images are embedded in one runtime call and each region in its own, cropped by the
runtime with the crop in its transform chain.

## Searching

| Method | Embeds |
|--------|--------|
| `search_text(cx, scope, text, query)` | The text, with the embedder's text tower: SigLIP 2 places texts and images in one space |
| `search_image(cx, scope, example, query)` | An example image |

`VisualQuery::top(k)` narrows with `captured_on(days)`, `kind(VisualKind::Frame)`,
`source("conveyor-1")` and `filter(..)`; every query also filters on the collection's space.
A day is a filter, not words in the text: "torn seals from Tuesday" is the text "a cardboard
box with a torn tape seal" and `captured_on([tuesday])`.

Each `VisualHit` carries the store's score, the caption, the artifact locator and URI, the
image's digest and media type, the region, the source, the kind, the capture time, the labels
and the run that stored it, so an agent can load the image (`MediaStore::load` checks the
digest) and cite the run.

## One space per collection

`open()` records the runtime embedder's `EmbeddingSpace` — model, revision, dimensions,
metric — in the collection itself: one record in the reserved scope `:adk-vision-space`,
which `Scope::from_parts` never produces. The record lives with the data, so it holds on every
backend and survives a restart on the persistent ones.

| When | Then |
|------|------|
| The collection is new | The space is recorded |
| The record matches | `open()` succeeds |
| The record names another model, revision, size or metric | `open()` fails with `VisionError::SpaceMismatch` (`vision.memory.space_mismatch`, invalid input), naming both spaces |
| The record changed after `open()` (another writer rebuilt the collection) | `index`, `search_text` and `search_image` fail the same way before they embed or write |

Changing the embedder means a new collection and indexing again; vectors of two spaces never
share a collection.

## Captions and graph memory

`captions(memory)` stores each caption as an `adk-memory` entry under the artifact's app and
user, with the chunk id as its memory id and as its memory session: `add_session` replaces a
session on some backends and appends on others, and one caption per session behaves the same
on every backend. The entry's content is the caption's
text and a `Part::FileData` reference to the artifact (`MemoryEntry::with_artifact`), so
keyword recall finds the caption by its words and returns the image's locator with it.

`adk-memory` keeps those references beside the text without changing what a search matches:
`adk_memory::text::extract_text` still returns the text parts alone, and
`artifact_references` returns the references — `Part::FileData`, and inline data or embedded
resources that name their source URI, never their bytes.

| `adk-memory` backend | Artifact references |
|----------------------|---------------------|
| In-memory, SQLite, PostgreSQL, MongoDB, Neo4j, Redis | Kept: they store the entry's whole content |
| Knowledge graph (episodic log) | Kept in a column of their own (migration 3) and returned as `Part::FileData` |
| Vertex AI Memory Bank | Not kept: Memory Bank stores the facts it extracts as text |

`graph(graph)` records each source as a `vision.source` entity of a `GraphMemoryService`,
with one observation per image (capture time, caption, artifact URI), related by `observed`
to a `vision.label` entity for each label.

## With an `adk-rag` pipeline

`SiglipTextEmbedding` adapts the runtime's embedder to `adk_rag::EmbeddingProvider`, and
`VisualMemory::text_embedding()` returns one for the memory's route. A `RagPipeline` or
`RagTool` built with it over the same collection answers a text question with images: each
result's text is a caption and its `source_uri` the image's artifact. Each embedding goes
through the runtime under the metering scope current at the call.

> **Note:** SigLIP 2's text-to-image cosines are small and often negative; give the pipeline a
> `similarity_threshold` below zero, or rank without one.

## Erasure

`erase_source(cx, scope, source)` removes everything stored from a source in a scope:

1. Lists the source's chunks (`document_id` = source) and deletes them.
2. Deletes each chunk's caption from `adk-memory`.
3. Deletes the source's graph entity, with its observations and relations.
4. Deletes each artifact no remaining chunk of the scope refers to; an artifact another
   source still uses is kept and reported.
5. Writes one `vision.memory.erased` audit record.

A frozen kill switch refuses indexing and searching but not erasure, so data can be erased
while agents are stopped. `Erasure` reports the chunks, captions, entities and artifacts
removed and the artifacts kept.

## Audit records

Both records have kind `vision`, the acting agent as actor, and digests and locators, never
pixels (V-I15).

| Action | Written by | Fields |
|--------|------------|--------|
| `vision.memory.indexed` | `index` | collection, scope, space, chunk count, image digests, sources, runs |
| `vision.memory.erased` | `erase_source` | collection, scope, source, chunks, captions, entities, image digests, artifacts deleted and kept |

## Backends

The visual memory uses only the `VectorStore` trait, so it runs on every backend. The suite in
`adk-vision/tests/memory_suite` (index, text and image queries, filters, scopes, captions,
graph, the pipeline, the space refusal across connections, erasure) runs on:

| Backend | Exercised |
|---------|-----------|
| `InMemoryVectorStore` | The suite (`memory_tests`, feature `memory`) and the S16 run |
| `LanceDBVectorStore` (embedded, persistent) | The suite with a new connection for every reopen (`memory_lancedb_tests`, feature `memory-lancedb`) and the S16 run |
| `PgVectorStore`, `QdrantVectorStore`, `AgentRetrievalStore` | Not by this suite; each passes `adk-rag`'s shared vector store contract, which covers every operation the memory uses |

## Scenario S16

`adk_vision::memory::fixtures::WeekCorpus` generates the scenario's week: 84 conveyor frames
from two cameras and 21 document pages (delivery notes, inspection reports, invoices) from
2026-11-16 to 2026-11-22, with torn seals seeded on Tuesday (3), Thursday (2) and Saturday (1)
and dented corners as distractors. Every picture is drawn from a seed and its data is
invented.

`memory_live_tests` indexes the week with SigLIP 2 base (quantized) and asks "torn seals from
Tuesday" by text and by an example torn seal that is not in the corpus. Recall@5 is the share
of Tuesday's three torn seals among the first five hits; the run fails below the baseline
recorded in `adk-vision/tests/fixtures/memory/s16-baseline.json`.

S16_RESULTS

```text
HF_HUB_CACHE=/path/to/models cargo nextest run -p adk-vision --features memory,onnx \
    --run-ignored only -E 'binary(memory_live_tests)' --no-capture
```

Add `memory-lancedb` to repeat the run on LanceDB; the answer must be the same.
