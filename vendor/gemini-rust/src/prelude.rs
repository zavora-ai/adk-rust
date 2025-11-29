//! # Gemini Rust SDK Prelude
//!
//! This module provides convenient imports for the most commonly used types
//! in the Gemini Rust SDK. Import everything with:
//!
//! ```rust
//! use gemini_rust::prelude::*;
//! ```
//!
//! This prelude includes only the essential types that most users will need.
//! For more specialized types, import them directly from the crate root or
//! their respective modules.

// Core client types
pub use crate::{ClientError, Gemini, Model};

// Builders for creating requests
pub use crate::{ContentBuilder, EmbedBuilder};

// Core data types for messages and content
pub use crate::{Content, Message, Role};

// Main response types
pub use crate::{ContentEmbeddingResponse, GenerationResponse};

// Configuration types
pub use crate::{GenerationConfig, TaskType};

// Safety settings
pub use crate::{HarmBlockThreshold, HarmCategory, SafetySetting};

// Function calling
pub use crate::{FunctionDeclaration, FunctionResponse, Tool};

// Batch and file handles (commonly used for async operations)
pub use crate::{Batch, FileHandle};
