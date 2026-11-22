//! [`SiglipTextEmbedding`]: the runtime's embedder as an `adk_rag::EmbeddingProvider`.

use std::sync::Arc;

use adk_rag::{EmbeddingProvider, RagError};
use async_trait::async_trait;

use crate::cx::VisionCx;
use crate::runtime::{CallOptions, VisionRuntime};
use crate::traits::EmbeddingSpace;

/// Text embeddings in the same space as a visual collection's images, as an
/// `adk_rag::EmbeddingProvider`.
///
/// SigLIP 2 (`onnx::SigLip2`, feature `onnx`) embeds texts and images into one space, so a [`RagPipeline`](adk_rag::RagPipeline) or [`RagTool`](adk_rag::RagTool)
/// built with this provider over a [`VisualMemory`](super::VisualMemory) collection finds
/// images by a text question: each result's text is the image's caption and its
/// `source_uri` the image's artifact.
///
/// Each call goes through the [`VisionRuntime`] under the metering scope current at the
/// call ([`VisionCx::current`]), so a tool's query is counted against its run's budget like
/// any other vision call. Any embedder whose texts and images share a space works; SigLIP 2
/// is the one the crate ships.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_rag::EmbeddingProvider;
/// use adk_vision::memory::SiglipTextEmbedding;
/// use adk_vision::{
///     BackendDescriptor, CallOptions, CoordFrame, Embedder, EmbeddingSpace, Locality, Metric,
///     SentImage, VisionCx, VisionRuntime,
/// };
///
/// struct Axis(BackendDescriptor, EmbeddingSpace);
///
/// #[async_trait::async_trait]
/// impl Embedder for Axis {
///     fn descriptor(&self) -> &BackendDescriptor {
///         &self.0
///     }
///     fn space(&self) -> &EmbeddingSpace {
///         &self.1
///     }
///     async fn embed_images(&self, _: &VisionCx, i: &[SentImage]) -> adk_vision::Result<Vec<Vec<f32>>> {
///         Ok(i.iter().map(|_| vec![1.0, 0.0]).collect())
///     }
///     async fn embed_texts(&self, _: &VisionCx, t: &[&str]) -> adk_vision::Result<Vec<Vec<f32>>> {
///         Ok(t.iter().map(|_| vec![0.0, 1.0]).collect())
///     }
/// }
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let space = EmbeddingSpace { model: "axis".into(), revision: "1".into(), dimensions: 2, metric: Metric::Cosine };
/// let descriptor = BackendDescriptor::new("axis", Locality::InProcess, CoordFrame::SentPixels);
/// let runtime = VisionRuntime::builder().embedder(Arc::new(Axis(descriptor, space))).build().unwrap();
/// let provider = SiglipTextEmbedding::new(Arc::new(runtime), CallOptions::default()).unwrap();
/// assert_eq!(provider.dimensions(), 2);
/// assert_eq!(provider.embed("a torn seal").await.unwrap(), vec![0.0, 1.0]);
/// # });
/// ```
#[derive(Clone)]
pub struct SiglipTextEmbedding {
    runtime: Arc<VisionRuntime>,
    options: CallOptions,
    space: EmbeddingSpace,
}

impl std::fmt::Debug for SiglipTextEmbedding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiglipTextEmbedding").field("space", &self.space).finish_non_exhaustive()
    }
}

impl SiglipTextEmbedding {
    /// The text embedder of `runtime`'s embedding route under `options`.
    ///
    /// # Errors
    ///
    /// [`VisionError::BackendNotFound`](crate::VisionError::BackendNotFound) without an
    /// embedder.
    pub fn new(runtime: Arc<VisionRuntime>, options: CallOptions) -> crate::Result<Self> {
        let space = runtime.embedding_space(&options)?;
        Ok(Self { runtime, options, space })
    }

    pub(super) fn from_parts(
        runtime: Arc<VisionRuntime>,
        options: CallOptions,
        space: EmbeddingSpace,
    ) -> Self {
        Self { runtime, options, space }
    }

    /// The space the vectors are in.
    pub fn space(&self) -> &EmbeddingSpace {
        &self.space
    }
}

#[async_trait]
impl EmbeddingProvider for SiglipTextEmbedding {
    async fn embed(&self, text: &str) -> adk_rag::Result<Vec<f32>> {
        self.embed_batch(&[text]).await?.pop().ok_or_else(|| RagError::EmbeddingError {
            provider: self.space.model.clone(),
            message: "the embedder returned no vector".into(),
        })
    }

    async fn embed_batch(&self, texts: &[&str]) -> adk_rag::Result<Vec<Vec<f32>>> {
        let current = self.runtime.embedding_space(&self.options).map_err(|e| {
            RagError::EmbeddingError { provider: self.space.model.clone(), message: e.to_string() }
        })?;
        if current != self.space {
            return Err(RagError::EmbeddingError {
                provider: self.space.model.clone(),
                message: format!(
                    "the runtime's embedder is now {}, not {}; vectors of two spaces never mix",
                    super::space_key(&current),
                    super::space_key(&self.space)
                ),
            });
        }
        self.runtime
            .embed_texts(&VisionCx::current(), texts, &self.options)
            .await
            .map(|out| out.value)
            .map_err(|e| RagError::EmbeddingError {
                provider: self.space.model.clone(),
                message: e.to_string(),
            })
    }

    fn dimensions(&self) -> usize {
        self.space.dimensions
    }
}
