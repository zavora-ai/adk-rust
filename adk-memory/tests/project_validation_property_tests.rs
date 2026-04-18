//! Property tests for project_id validation.
//!
//! Verifies that `validate_project_id` rejects empty strings and strings
//! longer than 256 characters, and accepts everything else.

use adk_memory::validate_project_id;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: project-scoped-memory, Property 4: Project ID Validation**
    ///
    /// *For any* string `s`, `validate_project_id(s)` rejects `s` if and only if
    /// `s` is empty or `s.len() > 256`. All other strings are accepted.
    ///
    /// **Validates: Requirements 14.1, 14.2, 14.3, 14.4**
    #[test]
    fn prop_project_id_validation(s in "\\PC{0,300}") {
        let result = validate_project_id(&s);

        if s.is_empty() {
            prop_assert!(
                result.is_err(),
                "Expected error for empty string, got Ok(())"
            );
        } else if s.len() > 256 {
            prop_assert!(
                result.is_err(),
                "Expected error for string of length {}, got Ok(())",
                s.len()
            );
        } else {
            prop_assert!(
                result.is_ok(),
                "Expected Ok(()) for valid string of length {}, got error: {:?}",
                s.len(),
                result.err()
            );
        }
    }

    /// **Feature: project-scoped-memory, Property 4b: Boundary — exactly 256 chars accepted**
    ///
    /// *For any* string of exactly 256 characters, validation succeeds.
    ///
    /// **Validates: Requirements 14.3**
    #[test]
    fn prop_project_id_boundary_256_accepted(c in proptest::char::range('a', 'z')) {
        let s: String = std::iter::repeat_n(c, 256).collect();
        let result = validate_project_id(&s);
        prop_assert!(
            result.is_ok(),
            "Expected Ok(()) for 256-char string, got error: {:?}",
            result.err()
        );
    }

    /// **Feature: project-scoped-memory, Property 4c: Boundary — 257 chars rejected**
    ///
    /// *For any* string of exactly 257 characters, validation fails.
    ///
    /// **Validates: Requirements 14.3, 14.4**
    #[test]
    fn prop_project_id_boundary_257_rejected(c in proptest::char::range('a', 'z')) {
        let s: String = std::iter::repeat_n(c, 257).collect();
        let result = validate_project_id(&s);
        prop_assert!(
            result.is_err(),
            "Expected error for 257-char string, got Ok(())"
        );
    }
}
