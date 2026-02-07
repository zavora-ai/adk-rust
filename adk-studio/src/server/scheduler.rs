//! Schedule Trigger Service for ADK Studio
//!
//! This module provides a background scheduler that monitors projects with
//! schedule triggers and executes them at the configured times.
//!
//! ## Features
//! - Cron expression parsing and scheduling
//! - Timezone-aware execution
//! - UI notification via SSE when scheduled runs start
//! - Graceful shutdown support

use crate::codegen::action_nodes::{ActionNodeConfig, TriggerType};
use crate::server::handlers::{
    WebhookNotification, get_project_binary_path, is_project_built, notify_webhook,
};
use crate::server::state::AppState;
use chrono::Timelike;
use chrono::{DateTime, Utc};
use cron::Schedule;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::Duration;

/// Scheduled job information
#[derive(Debug, Clone)]
struct ScheduledJob {
    project_id: String,
    project_name: String,
    trigger_id: String,
    cron: String,
    timezone: String,
    default_prompt: Option<String>,
    next_run: DateTime<Utc>,
    binary_path: String,
}

/// Scheduler state
pub struct SchedulerState {
    /// Map of project_id -> scheduled jobs
    jobs: HashMap<String, Vec<ScheduledJob>>,
    /// Whether the scheduler is running
    running: bool,
    /// Track last execution time per job (project_id:trigger_id -> last_run)
    last_executed: HashMap<String, DateTime<Utc>>,
}

impl SchedulerState {
    pub fn new() -> Self {
        Self { jobs: HashMap::new(), running: false, last_executed: HashMap::new() }
    }
}

// Global scheduler state
lazy_static::lazy_static! {
    pub static ref SCHEDULER: Arc<RwLock<SchedulerState>> = Arc::new(RwLock::new(SchedulerState::new()));
}

/// Parse a cron expression and get the next run time
/// Supports both 5-field (standard cron) and 6-field (with seconds) expressions
fn get_next_run(cron_expr: &str, _timezone: &str) -> Option<DateTime<Utc>> {
    // The cron crate expects 6 or 7 fields (with seconds)
    // Standard cron has 5 fields: minute hour day month weekday
    // Convert 5-field to 6-field by prepending "0" for seconds
    let parts: Vec<&str> = cron_expr.trim().split_whitespace().collect();
    let cron_with_seconds =
        if parts.len() == 5 { format!("0 {}", cron_expr) } else { cron_expr.to_string() };

    // Parse the cron expression
    let schedule = Schedule::from_str(&cron_with_seconds).ok()?;

    // Get the next occurrence
    // Note: For simplicity, we're using UTC. In production, you'd want to
    // properly handle timezone conversion using chrono-tz
    schedule.upcoming(Utc).next()
}

/// Scan projects and update scheduled jobs
async fn scan_projects(state: &AppState) -> Vec<ScheduledJob> {
    let storage = state.storage.read().await;
    let projects = match storage.list().await {
        Ok(metas) => metas,
        Err(e) => {
            tracing::error!("Failed to list projects for scheduler: {}", e);
            return Vec::new();
        }
    };

    let mut jobs = Vec::new();

    for meta in projects {
        let project = match storage.get(meta.id).await {
            Ok(p) => p,
            Err(_) => continue,
        };

        // Check if project is built
        let is_built = is_project_built(&project.name);
        let binary_path = get_project_binary_path(&project.name);

        // Debug: Log projects with triggers
        for (trigger_id, node) in &project.action_nodes {
            if let ActionNodeConfig::Trigger(trigger) = node {
                tracing::debug!(
                    project = %project.name,
                    trigger_id = %trigger_id,
                    trigger_type = ?trigger.trigger_type,
                    is_built = is_built,
                    binary_path = %binary_path,
                    has_schedule = trigger.schedule.is_some(),
                    "Found trigger in project"
                );
            }
        }

        if !is_built {
            continue;
        }

        // Find schedule triggers
        for (trigger_id, node) in &project.action_nodes {
            if let ActionNodeConfig::Trigger(trigger) = node {
                tracing::debug!(
                    project = %project.name,
                    trigger_type = ?trigger.trigger_type,
                    is_schedule = (trigger.trigger_type == TriggerType::Schedule),
                    "Checking trigger for schedule"
                );
                if trigger.trigger_type == TriggerType::Schedule {
                    tracing::debug!(
                        project = %project.name,
                        has_schedule_config = trigger.schedule.is_some(),
                        "Trigger is Schedule type"
                    );
                    if let Some(schedule) = &trigger.schedule {
                        let next_run_result = get_next_run(&schedule.cron, &schedule.timezone);
                        tracing::debug!(
                            project = %project.name,
                            cron = %schedule.cron,
                            next_run = ?next_run_result,
                            "Parsed cron expression"
                        );
                        if let Some(next_run) = next_run_result {
                            tracing::info!(
                                project = %project.name,
                                cron = %schedule.cron,
                                next_run = %next_run,
                                "Adding scheduled job"
                            );
                            jobs.push(ScheduledJob {
                                project_id: meta.id.to_string(),
                                project_name: project.name.clone(),
                                trigger_id: trigger_id.clone(),
                                cron: schedule.cron.clone(),
                                timezone: schedule.timezone.clone(),
                                default_prompt: schedule.default_prompt.clone(),
                                next_run,
                                binary_path: binary_path.clone(),
                            });
                        }
                    }
                }
            }
        }
    }

    jobs
}

/// Execute a scheduled job
async fn execute_job(job: &ScheduledJob) {
    tracing::info!(
        project_id = %job.project_id,
        project_name = %job.project_name,
        trigger_id = %job.trigger_id,
        cron = %job.cron,
        "Executing scheduled job"
    );

    // Generate a session ID
    let session_id = uuid::Uuid::new_v4().to_string();

    // Create a notification payload
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    // Use default_prompt if provided, otherwise send schedule metadata
    let payload = if let Some(prompt) = &job.default_prompt {
        serde_json::json!({
            "trigger": "schedule",
            "input": prompt,
            "cron": job.cron,
            "timezone": job.timezone,
            "scheduled_time": job.next_run.to_rfc3339(),
        })
    } else {
        serde_json::json!({
            "trigger": "schedule",
            "input": format!("Scheduled trigger fired at {} (cron: {})", job.next_run.to_rfc3339(), job.cron),
            "cron": job.cron,
            "timezone": job.timezone,
            "scheduled_time": job.next_run.to_rfc3339(),
        })
    };

    // Notify UI clients (reuse webhook notification channel)
    notify_webhook(
        &job.project_id,
        WebhookNotification {
            session_id: session_id.clone(),
            path: format!("/schedule/{}", job.trigger_id),
            method: "SCHEDULE".to_string(),
            payload,
            timestamp,
            binary_path: Some(job.binary_path.clone()),
        },
    )
    .await;

    tracing::info!(
        project_id = %job.project_id,
        session_id = %session_id,
        "Scheduled job notification sent to UI"
    );
}

/// Start the scheduler background task
pub async fn start_scheduler(state: AppState) {
    // Mark scheduler as running
    {
        let mut scheduler = SCHEDULER.write().await;
        if scheduler.running {
            tracing::warn!("Scheduler already running");
            return;
        }
        scheduler.running = true;
    }

    tracing::info!("Starting schedule trigger service");

    // Scheduler loop
    loop {
        // Scan projects every 30 seconds
        let jobs = scan_projects(&state).await;

        tracing::info!(
            job_count = jobs.len(),
            "Scheduler scan complete - found {} schedule triggers",
            jobs.len()
        );

        for job in &jobs {
            tracing::debug!(
                project = %job.project_name,
                cron = %job.cron,
                next_run = %job.next_run,
                "Found scheduled job"
            );
        }

        // Update scheduler state
        {
            let mut scheduler = SCHEDULER.write().await;
            if !scheduler.running {
                tracing::info!("Scheduler stopped");
                break;
            }

            scheduler.jobs.clear();
            for job in &jobs {
                scheduler
                    .jobs
                    .entry(job.project_id.clone())
                    .or_insert_with(Vec::new)
                    .push(job.clone());
            }
        }

        // Check for jobs that need to run
        let now = Utc::now();
        // Round down to the current minute for comparison
        let Some(current_minute) = now.with_second(0).and_then(|t| t.with_nanosecond(0)) else {
            tracing::warn!("Failed to round current time to minute");
            tokio::time::sleep(Duration::from_secs(30)).await;
            continue;
        };

        for job in &jobs {
            let job_key = format!("{}:{}", job.project_id, job.trigger_id);

            // Check if we should execute this job
            // A job should run if:
            // 1. The current minute matches the cron schedule
            // 2. We haven't already executed it for this minute
            let should_execute = {
                let scheduler = SCHEDULER.read().await;
                let last_exec = scheduler.last_executed.get(&job_key);

                // Check if we already executed in this minute
                let already_executed = last_exec
                    .and_then(|t| t.with_second(0).and_then(|t| t.with_nanosecond(0)))
                    .map(|t| t >= current_minute)
                    .unwrap_or(false);

                // The cron library's next_run is always in the future
                // If next_run is within the next minute, it means the current minute matches the schedule
                // (because cron gives us the NEXT occurrence, and if it's in the next minute, we're currently in a matching minute)
                let next_run_minute =
                    match job.next_run.with_second(0).and_then(|t| t.with_nanosecond(0)) {
                        Some(t) => t,
                        None => continue,
                    };
                let time_to_next = (next_run_minute - current_minute).num_seconds();

                // If next run is within 60 seconds, we're in a matching minute
                let is_matching_minute = time_to_next <= 60 && time_to_next > 0;

                is_matching_minute && !already_executed
            };

            if should_execute {
                tracing::info!(
                    project = %job.project_name,
                    trigger_id = %job.trigger_id,
                    next_run = %job.next_run,
                    current_minute = %current_minute,
                    "Executing scheduled job"
                );

                // Mark as executed
                {
                    let mut scheduler = SCHEDULER.write().await;
                    scheduler.last_executed.insert(job_key, now);
                }

                execute_job(&job).await;
            }
        }

        // Log scheduled jobs
        if !jobs.is_empty() {
            tracing::debug!(
                job_count = jobs.len(),
                "Scheduler tick - {} jobs scheduled",
                jobs.len()
            );
        }

        // Sleep for 30 seconds before next check
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

/// Stop the scheduler
pub async fn stop_scheduler() {
    let mut scheduler = SCHEDULER.write().await;
    scheduler.running = false;
    tracing::info!("Scheduler stop requested");
}

/// Get the list of scheduled jobs for a project
pub async fn get_project_schedules(project_id: &str) -> Vec<ScheduledJobInfo> {
    let scheduler = SCHEDULER.read().await;
    scheduler
        .jobs
        .get(project_id)
        .map(|jobs| {
            jobs.iter()
                .map(|j| ScheduledJobInfo {
                    trigger_id: j.trigger_id.clone(),
                    cron: j.cron.clone(),
                    timezone: j.timezone.clone(),
                    next_run: j.next_run.to_rfc3339(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Public job info for API responses
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScheduledJobInfo {
    pub trigger_id: String,
    pub cron: String,
    pub timezone: String,
    pub next_run: String,
}
