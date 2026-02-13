use snafu::Snafu;

pub mod builder;
pub use builder::CacheBuilder;
pub mod handle;
pub use handle::CachedContentHandle;
pub mod model;

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("client invocation error"))]
    Client { source: Box<crate::error::Error> },

    #[snafu(display(
        "cache display name ('{display_name}') too long ({chars}), must be under 128 characters"
    ))]
    LongDisplayName { display_name: String, chars: usize },

    #[snafu(display("expiration (TTL or expire time) is required for cache creation"))]
    MissingExpiration,
}
