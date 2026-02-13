use snafu::Snafu;

pub mod builder;
pub use builder::BatchBuilder;
pub mod handle;
pub use handle::*;
pub mod model;

#[derive(Debug, Snafu)]
pub enum Error {
    Client { source: crate::error::Error },
    File { source: crate::files::Error },
    Serialize { source: serde_json::Error },
}
