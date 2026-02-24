//! Composable audio pipeline system.

pub mod builder;
pub mod chunker;
pub mod handle;
pub mod presets;
pub mod types;
pub(crate) mod voice_agent;

pub use builder::AudioPipelineBuilder;
pub use chunker::SentenceChunker;
pub use handle::PipelineHandle;
pub use presets::*;
pub use types::*;
