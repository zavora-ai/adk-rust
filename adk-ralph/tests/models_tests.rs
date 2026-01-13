//! Tests for Ralph data models.

use adk_ralph::{
    AcceptanceCriterion, Component, DesignDocument, FileStructure, Phase, PrdDocument,
    ProgressEntry, ProgressLog, Sprint, Task, TaskList, TaskStatus, TechnologyStack, TestResults,
    UserStory,
};

mod prd_tests {
    use super::*;

    #[test]
    fn test_user_story_lifecycle() {
        let mut story = UserStory::new(
            "US-001",
            "Test Feature",
            "As a user, I want to test features",
            1,
        );

        assert!(story.is_pending());
        assert!(!story.is_complete());

        story.mark_in_progress();
        assert!(!story.is_pending());

        story.mark_passing();
        assert!(story.is_complete());
    }

    #[test]
    fn test_prd_document_validation() {
        let mut prd = PrdDocument::new("Test Project", "Test overview");

        // Should fail without user stories
        assert!(prd.validate().is_err());

        // Add a valid user story
        let mut story = UserStory::new("US-001", "Story", "Description", 1);
        story.add_criterion_text("WHEN x THEN y");
        prd.add_user_story(story);

        assert!(prd.validate().is_ok());
    }

    #[test]
    fn test_prd_stats() {
        let mut prd = PrdDocument::new("Test", "Overview");

        let mut story1 = UserStory::new("US-001", "S1", "D1", 1);
        story1.add_criterion_text("C1");
        story1.mark_passing();

        let mut story2 = UserStory::new("US-002", "S2", "D2", 2);
        story2.add_criterion_text("C2");

        prd.add_user_story(story1);
        prd.add_user_story(story2);

        let stats = prd.get_stats();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.completed, 1);
        assert_eq!(stats.remaining, 1);
        assert!((stats.completion_rate - 50.0).abs() < 0.1);
    }

    #[test]
    fn test_acceptance_criterion() {
        let mut criterion = AcceptanceCriterion::new("1", "WHEN user clicks THEN action occurs");
        assert!(!criterion.verified);

        criterion.verify();
        assert!(criterion.verified);
    }
}

mod design_tests {
    use super::*;

    #[test]
    fn test_component_creation() {
        let mut component = Component::new("AuthService", "Handles authentication");
        component.add_interface("fn login(user: &str, pass: &str) -> Result<Token>");
        component.add_dependency("DatabaseService");

        assert_eq!(component.name, "AuthService");
        assert_eq!(component.interface.len(), 1);
        assert_eq!(component.dependencies.len(), 1);
    }

    #[test]
    fn test_file_structure_tree() {
        let mut root = FileStructure::directory("project", "Root");
        let mut src = FileStructure::directory("src", "Source");
        src.add_child(FileStructure::file("main.rs", "Entry"));
        src.add_child(FileStructure::file("lib.rs", "Library"));
        root.add_child(src);
        root.add_child(FileStructure::file("Cargo.toml", "Manifest"));

        let tree = root.to_tree("", true);
        assert!(tree.contains("src/"));
        assert!(tree.contains("main.rs"));
        assert!(tree.contains("Cargo.toml"));
    }

    #[test]
    fn test_technology_stack() {
        let mut tech = TechnologyStack::new("rust")
            .with_testing("cargo test")
            .with_build_tool("cargo");
        tech.add_dependency("tokio");
        tech.add_dependency("serde");

        assert_eq!(tech.language, "rust");
        assert_eq!(tech.dependencies.len(), 2);
    }

    #[test]
    fn test_design_document_completeness() {
        let mut design = DesignDocument::new("Test", "Overview");

        // Initially incomplete
        assert!(!design.is_complete());

        // Add required sections
        design.set_diagram("flowchart TB\n  A --> B");
        design.add_component(Component::new("Main", "Entry point"));
        design.set_file_structure(FileStructure::directory("project", "Root"));
        design.set_technology_stack(TechnologyStack::new("rust"));

        assert!(design.is_complete());
    }
}

mod tasks_tests {
    use super::*;

    #[test]
    fn test_task_lifecycle() {
        let mut task = Task::new("TASK-001", "Implement feature", "Description", 1);

        assert!(task.is_pending());
        assert_eq!(task.attempts, 0);

        task.start();
        assert_eq!(task.status, TaskStatus::InProgress);
        assert_eq!(task.attempts, 1);

        task.complete(Some("abc123".to_string()));
        assert!(task.is_completed());
        assert_eq!(task.commit_hash, Some("abc123".to_string()));
    }

    #[test]
    fn test_task_blocking() {
        let mut task = Task::new("TASK-001", "Task", "Desc", 1);
        task.block("Dependency not met");

        assert!(task.is_blocked());
        assert!(task.notes.contains("Blocked"));
    }

    #[test]
    fn test_task_list_priority_selection() {
        let mut list = TaskList::new("Test", "rust");

        // Add tasks with different priorities
        list.add_task(Task::new("TASK-001", "Low priority", "Desc", 3));
        list.add_task(Task::new("TASK-002", "High priority", "Desc", 1));
        list.add_task(Task::new("TASK-003", "Medium priority", "Desc", 2));

        // Should select highest priority (lowest number)
        let next = list.get_next_task().unwrap();
        assert_eq!(next.id, "TASK-002");
    }

    #[test]
    fn test_task_list_dependency_handling() {
        let mut list = TaskList::new("Test", "rust");

        let task1 = Task::new("TASK-001", "First", "Desc", 1);
        let mut task2 = Task::new("TASK-002", "Second", "Desc", 1);
        task2.add_dependency("TASK-001");

        list.add_task(task1);
        list.add_task(task2);

        // TASK-002 has same priority but depends on TASK-001
        let next = list.get_next_task().unwrap();
        assert_eq!(next.id, "TASK-001");

        // Complete TASK-001
        list.complete_task("TASK-001", None).unwrap();

        // Now TASK-002 should be selectable
        let next = list.get_next_task().unwrap();
        assert_eq!(next.id, "TASK-002");
    }

    #[test]
    fn test_sprint_completion() {
        let mut sprint = Sprint::new("sprint-1", "Foundation");

        let mut task1 = Task::new("TASK-001", "T1", "D1", 1);
        task1.complete(None);
        sprint.add_task(task1);

        let task2 = Task::new("TASK-002", "T2", "D2", 2);
        sprint.add_task(task2);

        assert!(!sprint.is_complete());
        assert!((sprint.completion_percentage() - 50.0).abs() < 0.1);
    }

    #[test]
    fn test_phase_structure() {
        let mut phase = Phase::new("phase-1", "Core Implementation");
        let mut sprint = Sprint::new("sprint-1", "Foundation");
        sprint.add_task(Task::new("TASK-001", "Task", "Desc", 1));
        phase.add_sprint(sprint);

        let tasks = phase.get_all_tasks();
        assert_eq!(tasks.len(), 1);
    }
}

mod progress_tests {
    use super::*;

    #[test]
    fn test_progress_entry_creation() {
        let mut entry = ProgressEntry::new("TASK-001", "Test Task", 1, "Used TDD approach");

        entry.add_learning("Always write tests first");
        entry.add_gotcha("Watch out for async issues");
        entry.add_file_created("src/main.rs");

        assert_eq!(entry.learnings.len(), 1);
        assert_eq!(entry.gotchas.len(), 1);
        assert_eq!(entry.files_created.len(), 1);
    }

    #[test]
    fn test_progress_log_append_only() {
        let mut log = ProgressLog::new("Test Project");

        let entry1 = ProgressEntry::new("TASK-001", "First", 1, "Approach 1");
        log.append(entry1);
        assert_eq!(log.entry_count(), 1);

        let entry2 = ProgressEntry::new("TASK-002", "Second", 2, "Approach 2");
        log.append(entry2);
        assert_eq!(log.entry_count(), 2);

        // Verify order is preserved
        assert_eq!(log.entries[0].task_id, "TASK-001");
        assert_eq!(log.entries[1].task_id, "TASK-002");
    }

    #[test]
    fn test_test_results() {
        let results = TestResults::new(10, 2, 1);
        assert!(!results.all_passed());
        assert_eq!(results.total(), 13);

        let passing = TestResults::new(10, 0, 0);
        assert!(passing.all_passed());
    }

    #[test]
    fn test_progress_summary() {
        let mut log = ProgressLog::new("Test");

        let mut entry = ProgressEntry::new("TASK-001", "Test", 1, "Approach");
        entry.files_created = vec!["file1.rs".to_string()];
        entry.test_results = Some(TestResults::new(5, 0, 0));
        entry.commit_hash = Some("abc123".to_string());
        log.append(entry);

        log.update_summary(3);

        assert_eq!(log.summary.tasks_completed, 1);
        assert_eq!(log.summary.tasks_remaining, 3);
        assert_eq!(log.summary.total_commits, 1);
        assert_eq!(log.summary.total_files_created, 1);
        assert_eq!(log.summary.total_tests_passed, 5);
    }

    #[test]
    fn test_get_all_learnings() {
        let mut log = ProgressLog::new("Test");

        let mut entry1 = ProgressEntry::new("TASK-001", "T1", 1, "A1");
        entry1.add_learning("Learning 1");
        entry1.add_learning("Learning 2");
        log.append(entry1);

        let mut entry2 = ProgressEntry::new("TASK-002", "T2", 2, "A2");
        entry2.add_learning("Learning 3");
        log.append(entry2);

        let learnings = log.get_all_learnings();
        assert_eq!(learnings.len(), 3);
    }
}
