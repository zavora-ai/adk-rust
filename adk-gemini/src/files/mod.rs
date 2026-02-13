use snafu::Snafu;

pub mod builder;
pub mod handle;
pub mod model;

#[derive(Debug, Snafu)]
pub enum Error {
    Client { source: crate::error::Error },
}
