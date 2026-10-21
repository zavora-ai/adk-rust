//! The encrypted store conformance suite over every wrapped store of a `harness.db` file,
//! including the scan of the file for the fixture's sentinels (spec 3.1 §7.12, §9.2 I57,
//! §11).

#![cfg(all(feature = "encrypted", feature = "sqlite", feature = "memory", feature = "runner"))]

adk_harness::encrypted_store_conformance_tests!(
    keyring = adk_harness::encrypted::StoreKeyring::generate()
);
