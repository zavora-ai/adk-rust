use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DeployError {
    #[error("deployment manifest not found at {path}")]
    ManifestNotFound { path: PathBuf },

    #[error("invalid deployment manifest: {message}")]
    InvalidManifest { message: String },

    #[error("failed to parse deployment manifest: {message}")]
    ManifestParse { message: String },

    #[error(
        "bundle build failed: {message}. Try running `cargo build --release` directly for more details."
    )]
    BundleBuild { message: String },

    #[error("control-plane request failed: {message}")]
    Client { message: String },

    #[error("failed to persist deploy config: {message}")]
    Config { message: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
}

pub type DeployResult<T> = Result<T, DeployError>;
