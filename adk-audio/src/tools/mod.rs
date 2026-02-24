//! Audio tools for LlmAgent integration.

mod apply_fx;
mod generate_music;
mod speak;
mod transcribe;

pub use apply_fx::ApplyFxTool;
pub use generate_music::GenerateMusicTool;
pub use speak::SpeakTool;
pub use transcribe::TranscribeTool;
