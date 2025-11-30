pub mod inmemory;
pub mod scoped;
pub mod service;

pub use inmemory::InMemoryArtifactService;
pub use scoped::ScopedArtifacts;
pub use service::{
    ArtifactService, DeleteRequest, ListRequest, ListResponse, LoadRequest, LoadResponse,
    SaveRequest, SaveResponse, VersionsRequest, VersionsResponse,
};
