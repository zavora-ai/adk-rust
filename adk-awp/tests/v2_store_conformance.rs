//! The in-memory edge stores pass the edge store conformance suite (spec 3.5 §7.9, §11).

adk_awp::edge_store_conformance_tests! {
    harness = adk_awp::v2::store::MemoryEdgeStores::default(),
}
