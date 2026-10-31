//! The integration tests of `adk-harness`, built as one test binary so the dependency tree links
//! once rather than once per file. Each `tests/*.rs` file is a module named after it.

// The shared helpers compile only with the features of the files that use them; the harness
// fixture in `common` needs `runner`.
#[cfg(feature = "runner")]
mod common;
#[cfg(all(feature = "runner", any(feature = "postgres", feature = "redis")))]
mod replicas;
#[cfg(feature = "website")]
mod website_common;

mod acp_standing_tests;
mod action_ledger_tests;
mod agent_engine_tests;
mod agents_harness;
mod apply_patch_policy_tests;
mod approvals_property_tests;
mod background_shell_tests;
mod budget_tests;
mod change_set_tests;
mod checkpoint_store_tests;
mod close_tests;
mod confirmation_wait_tests;
mod consolidation_kernel_tests;
mod consolidation_property_tests;
mod consolidation_tests;
mod container_shell_tests;
mod decide_settled_tests;
mod devtools_error_tests;
mod durability_tests;
mod encrypted_store_tests;
mod eval_suite;
mod extended_protected_tests;
mod fact_conformance_tests;
mod facts_property_tests;
mod freeze_prompt_tests;
mod goal_tests;
mod governance_binding_tests;
mod harness_scenarios;
mod hooks_property_tests;
mod hooks_tests;
mod identity_tests;
mod kernel_driver_tests;
mod kernel_partition_drivers;
mod kernel_partitions;
mod kernel_tests;
mod long_term_memory_tests;
mod managed_kernel_tests;
mod managed_partition_drivers;
mod managed_tests;
mod mcp_interaction_tests;
mod memory_tests;
mod mode_change_inbox_tests;
mod mode_mirror_tests;
mod observation_kernel_tests;
mod observe_tests;
mod operator_policy_tests;
mod pack_browser_tests;
mod pack_computer_use_tests;
mod pack_managed_tests;
mod payments_tests;
mod personal_preset_tests;
mod plan_graph_tests;
mod policy_table_tests;
mod posture_tests;
mod prompt_turn_tests;
mod requested_call_tests;
mod review_board_tests;
mod rule_hit_tests;
mod self_knowledge_tests;
mod shared_store_postgres_tests;
mod shared_store_redis_tests;
mod shell_sandbox_tests;
mod shell_sandbox_windows_tests;
mod skill_audit_tests;
mod snapshot_rewind_tests;
mod store_conformance_tests;
mod symlink_protection_tests;
mod take_over_tests;
mod team_tests;
mod todo_tests;
mod voice_kernel_tests;
mod voice_pack_tests;
mod web_property_tests;
mod web_search_default_tests;
mod web_tests;
mod website_drafts_tests;
mod website_ladder_tests;
mod website_live_tests;
mod website_onboarding_tests;
mod website_scenarios;
mod work_stays_at_work;
mod workspace_trust_tests;
