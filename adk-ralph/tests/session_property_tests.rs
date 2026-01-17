//! Property-based tests for Session management in Ralph interactive mode.
//!
//! These tests validate the correctness properties for session state management,
//! conversation history preservation, and user preferences persistence.

use adk_ralph::{Message, ProjectContext, Session};
use proptest::prelude::*;
use std::collections::HashMap;
use tempfile::TempDir;

// ============================================================================
// Generators for Session types
// ============================================================================

/// Generate a valid role string
fn arb_role() -> impl Strategy<Value = String> {
    prop_oneof![Just("user".to_string()), Just("assistant".to_string()),]
}

/// Generate a valid message content (non-empty, reasonable length)
fn arb_message_content() -> impl Strategy<Value = String> {
    "[A-Za-z0-9 .,!?]{1,200}".prop_map(|s| s.trim().to_string())
}

/// Generate a valid Message
fn arb_message() -> impl Strategy<Value = Message> {
    (arb_role(), arb_message_content()).prop_map(|(role, content)| Message::new(role, content))
}

/// Generate a sequence of messages (conversation history)
fn arb_conversation_history() -> impl Strategy<Value = Vec<Message>> {
    prop::collection::vec(arb_message(), 0..20)
}

/// Generate a valid project path
fn arb_project_path() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("/tmp/test-project".to_string()),
        Just("/home/user/projects/myapp".to_string()),
        Just("./my-project".to_string()),
        Just("/var/projects/app".to_string()),
    ]
}

/// Generate a valid preference key
fn arb_preference_key() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("auto_approve".to_string()),
        Just("preferred_mode".to_string()),
        Just("theme".to_string()),
        Just("max_iterations".to_string()),
        Just("verbose".to_string()),
    ]
}

/// Generate a valid preference value
fn arb_preference_value() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("true".to_string()),
        Just("false".to_string()),
        Just("incremental".to_string()),
        Just("pipeline".to_string()),
        Just("dark".to_string()),
        Just("light".to_string()),
        Just("50".to_string()),
        Just("100".to_string()),
    ]
}

/// Generate a map of user preferences
fn arb_user_preferences() -> impl Strategy<Value = HashMap<String, String>> {
    prop::collection::hash_map(arb_preference_key(), arb_preference_value(), 0..5)
}

/// Generate a valid ProjectContext
#[allow(dead_code)]
fn arb_project_context() -> impl Strategy<Value = ProjectContext> {
    (
        arb_project_path(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        prop::option::of(prop_oneof![
            Just("rust".to_string()),
            Just("python".to_string()),
            Just("go".to_string()),
            Just("typescript".to_string()),
        ]),
    )
        .prop_map(|(path, has_prd, has_design, has_tasks, language)| {
            let mut ctx = ProjectContext::new(path);
            ctx.has_prd = has_prd;
            ctx.has_design = has_design;
            ctx.has_tasks = has_tasks;
            ctx.language = language;
            ctx
        })
}

// ============================================================================
// Property Tests
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    // ========================================================================
    // Property 2: Conversation History Preservation
    // ========================================================================

    /// **Feature: ralph-interactive-mode, Property 2: Conversation History Preservation**
    /// *For any* sequence of user messages added to a session, the session's conversation
    /// history SHALL contain all messages in order, and context queries SHALL be able to
    /// reference earlier messages.
    /// **Validates: Requirements 1.4, 5.1**
    #[test]
    fn prop_conversation_history_preservation(
        project_path in arb_project_path(),
        messages in arb_conversation_history()
    ) {
        let mut session = Session::new(&project_path);

        // Add all messages to the session
        for msg in &messages {
            session.add_message(&msg.role, &msg.content);
        }

        // Property 1: All messages should be present
        prop_assert_eq!(
            session.conversation_history.len(),
            messages.len(),
            "Session should contain all added messages"
        );

        // Property 2: Messages should be in order
        for (i, (session_msg, original_msg)) in session
            .conversation_history
            .iter()
            .zip(messages.iter())
            .enumerate()
        {
            prop_assert_eq!(
                &session_msg.role,
                &original_msg.role,
                "Message {} role should match",
                i
            );
            prop_assert_eq!(
                &session_msg.content,
                &original_msg.content,
                "Message {} content should match",
                i
            );
        }

        // Property 3: Context summary should reference messages if any exist
        let summary = session.get_context_summary();
        if !messages.is_empty() {
            prop_assert!(
                summary.contains("messages"),
                "Context summary should mention messages when history is non-empty"
            );
        }
    }

    /// **Feature: ralph-interactive-mode, Property 2a: Message Order Preservation**
    /// *For any* sequence of alternating user/assistant messages, the order SHALL be
    /// preserved exactly as added.
    /// **Validates: Requirements 1.4, 5.1**
    #[test]
    fn prop_message_order_preservation(
        project_path in arb_project_path(),
        message_count in 1usize..10
    ) {
        let mut session = Session::new(&project_path);

        // Add alternating user/assistant messages
        for i in 0..message_count {
            if i % 2 == 0 {
                session.add_user_message(&format!("User message {}", i));
            } else {
                session.add_assistant_message(&format!("Assistant message {}", i));
            }
        }

        // Verify order is preserved
        for (i, msg) in session.conversation_history.iter().enumerate() {
            let expected_role = if i % 2 == 0 { "user" } else { "assistant" };
            prop_assert_eq!(
                &msg.role,
                expected_role,
                "Message {} should have role '{}'",
                i,
                expected_role
            );
            prop_assert!(
                msg.content.contains(&format!("{}", i)),
                "Message {} should contain its index",
                i
            );
        }
    }

    /// **Feature: ralph-interactive-mode, Property 2b: Context Reference Resolution**
    /// *For any* session with messages, the context summary SHALL be able to reference
    /// recent messages for context resolution.
    /// **Validates: Requirements 5.1, 5.2**
    #[test]
    fn prop_context_reference_resolution(
        project_path in arb_project_path(),
        messages in prop::collection::vec(arb_message_content(), 1..5)
    ) {
        let mut session = Session::new(&project_path);

        // Add messages
        for (i, content) in messages.iter().enumerate() {
            if i % 2 == 0 {
                session.add_user_message(content);
            } else {
                session.add_assistant_message(content);
            }
        }

        // Get context summary
        let summary = session.get_context_summary();

        // The summary should contain recent message information
        prop_assert!(
            summary.contains("Recent messages") || summary.contains("messages"),
            "Context summary should reference recent messages"
        );

        // The last message content (or truncated version) should be findable in context
        if let Some(last_msg) = session.conversation_history.last() {
            // Either the full content or a truncated version should be in the summary
            let content_preview = if last_msg.content.len() > 100 {
                &last_msg.content[..100]
            } else {
                &last_msg.content
            };
            prop_assert!(
                summary.contains(content_preview) || summary.contains(&last_msg.role),
                "Context summary should include recent message content or role"
            );
        }
    }

    // ========================================================================
    // Property 8: User Preferences Persistence
    // ========================================================================

    /// **Feature: ralph-interactive-mode, Property 8: User Preferences Persistence**
    /// *For any* user preference set during a session, that preference SHALL be
    /// retrievable in subsequent turns and after session reload.
    /// **Validates: Requirements 5.4**
    #[test]
    fn prop_user_preferences_persistence(
        preferences in arb_user_preferences()
    ) {
        // Create a temporary directory for session persistence
        let temp_dir = TempDir::new().unwrap();
        let session_path = temp_dir.path().join("session.json");

        // Create session and set preferences
        let mut session = Session::new(temp_dir.path());
        for (key, value) in &preferences {
            session.set_preference(key, value);
        }

        // Property 1: All preferences should be retrievable immediately
        for (key, value) in &preferences {
            let retrieved = session.get_preference(key);
            prop_assert_eq!(
                retrieved,
                Some(value),
                "Preference '{}' should be retrievable with value '{}'",
                key,
                value
            );
        }

        // Property 2: Preferences should appear in context summary
        let summary = session.get_context_summary();
        if !preferences.is_empty() {
            prop_assert!(
                summary.contains("Preferences"),
                "Context summary should include preferences section when preferences exist"
            );
        }

        // Property 3: Preferences should persist after save/load
        session.save(&session_path).unwrap();
        let loaded_session = Session::load(&session_path).unwrap();

        for (key, value) in &preferences {
            let retrieved = loaded_session.get_preference(key);
            prop_assert_eq!(
                retrieved,
                Some(value),
                "Preference '{}' should persist after reload with value '{}'",
                key,
                value
            );
        }
    }

    /// **Feature: ralph-interactive-mode, Property 8a: Preference Update Persistence**
    /// *For any* preference that is updated, the new value SHALL replace the old value
    /// and persist correctly.
    /// **Validates: Requirements 5.4**
    #[test]
    fn prop_preference_update_persistence(
        key in arb_preference_key(),
        initial_value in arb_preference_value(),
        updated_value in arb_preference_value()
    ) {
        let temp_dir = TempDir::new().unwrap();
        let session_path = temp_dir.path().join("session.json");

        let mut session = Session::new(temp_dir.path());

        // Set initial value
        session.set_preference(&key, &initial_value);
        prop_assert_eq!(
            session.get_preference(&key),
            Some(&initial_value),
            "Initial preference should be set"
        );

        // Update value
        session.set_preference(&key, &updated_value);
        prop_assert_eq!(
            session.get_preference(&key),
            Some(&updated_value),
            "Updated preference should replace initial value"
        );

        // Save and reload
        session.save(&session_path).unwrap();
        let loaded_session = Session::load(&session_path).unwrap();

        prop_assert_eq!(
            loaded_session.get_preference(&key),
            Some(&updated_value),
            "Updated preference should persist after reload"
        );
    }

    /// **Feature: ralph-interactive-mode, Property 8b: Preference Removal**
    /// *For any* preference that is removed, it SHALL no longer be retrievable.
    /// **Validates: Requirements 5.4**
    #[test]
    fn prop_preference_removal(
        key in arb_preference_key(),
        value in arb_preference_value()
    ) {
        let temp_dir = TempDir::new().unwrap();
        let session_path = temp_dir.path().join("session.json");

        let mut session = Session::new(temp_dir.path());

        // Set and verify preference exists
        session.set_preference(&key, &value);
        prop_assert!(
            session.get_preference(&key).is_some(),
            "Preference should exist after setting"
        );

        // Remove preference
        let removed = session.remove_preference(&key);
        prop_assert_eq!(
            removed,
            Some(value.clone()),
            "Remove should return the removed value"
        );

        // Verify preference is gone
        prop_assert!(
            session.get_preference(&key).is_none(),
            "Preference should not exist after removal"
        );

        // Save and reload - preference should still be gone
        session.save(&session_path).unwrap();
        let loaded_session = Session::load(&session_path).unwrap();

        prop_assert!(
            loaded_session.get_preference(&key).is_none(),
            "Removed preference should not reappear after reload"
        );
    }
}

// ============================================================================
// Additional Unit Tests for Edge Cases
// ============================================================================

#[cfg(test)]
mod edge_case_tests {
    use super::*;

    #[test]
    fn test_empty_session_history() {
        let session = Session::new("/test");
        assert!(session.conversation_history.is_empty());
        let summary = session.get_context_summary();
        assert!(!summary.contains("Recent messages"));
    }

    #[test]
    fn test_empty_preferences() {
        let session = Session::new("/test");
        assert!(session.user_preferences.is_empty());
        let summary = session.get_context_summary();
        assert!(!summary.contains("Preferences"));
    }

    #[test]
    fn test_message_with_special_characters() {
        let mut session = Session::new("/test");
        let special_content = "Hello! How's it going? 🎉 <script>alert('test')</script>";
        session.add_user_message(special_content);

        assert_eq!(session.conversation_history.len(), 1);
        assert_eq!(session.conversation_history[0].content, special_content);
    }

    #[test]
    fn test_preference_with_empty_value() {
        let mut session = Session::new("/test");
        session.set_preference("key", "");

        assert_eq!(session.get_preference("key"), Some(&"".to_string()));
    }

    #[test]
    fn test_large_conversation_history() {
        let mut session = Session::new("/test");

        // Add 100 messages
        for i in 0..100 {
            session.add_user_message(&format!("Message {}", i));
        }

        assert_eq!(session.conversation_history.len(), 100);

        // Verify order is preserved
        for (i, msg) in session.conversation_history.iter().enumerate() {
            assert!(msg.content.contains(&format!("{}", i)));
        }
    }

    #[test]
    fn test_session_updated_at_changes() {
        let mut session = Session::new("/test");
        let initial_updated = session.updated_at;

        // Small delay to ensure timestamp difference
        std::thread::sleep(std::time::Duration::from_millis(10));

        session.add_user_message("test");
        assert!(session.updated_at > initial_updated);
    }

    #[test]
    fn test_preference_updated_at_changes() {
        let mut session = Session::new("/test");
        let initial_updated = session.updated_at;

        // Small delay to ensure timestamp difference
        std::thread::sleep(std::time::Duration::from_millis(10));

        session.set_preference("key", "value");
        assert!(session.updated_at > initial_updated);
    }
}
