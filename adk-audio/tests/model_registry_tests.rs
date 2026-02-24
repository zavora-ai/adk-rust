//! LocalModelRegistry tests.
//!
//! **Validates: Requirement 18**

use adk_audio::LocalModelRegistry;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Same model_id always produces the same path
    #[test]
    fn prop_path_determinism(model_id in "[a-z]{3,10}/[a-z_-]{3,15}") {
        let registry = LocalModelRegistry::new("/tmp/test-cache");
        let path1 = registry.model_path(&model_id);
        let path2 = registry.model_path(&model_id);
        prop_assert_eq!(path1, path2);
    }

    /// Slashes in model_id are replaced with double-dash
    #[test]
    fn prop_path_sanitization(org in "[a-z]{3,10}", name in "[a-z_-]{3,15}") {
        let model_id = format!("{org}/{name}");
        let registry = LocalModelRegistry::new("/tmp/test-cache");
        let path = registry.model_path(&model_id);
        let path_str = path.to_string_lossy();
        // The path should not contain a slash in the model directory name
        let filename = path.file_name().unwrap().to_string_lossy();
        prop_assert!(!filename.contains('/'), "path should not contain slash: {filename}");
        let expected = format!("{}--{}", org, name);
        prop_assert!(path_str.contains(&expected));
    }

    /// Empty model_id returns error from get_or_download
    #[test]
    fn prop_empty_model_id_error(_dummy in 0..1u8) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let registry = LocalModelRegistry::new("/tmp/test-cache");
            let result = registry.get_or_download("").await;
            prop_assert!(result.is_err());
            Ok(())
        })?;
    }
}
