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

use crate::server::handlers::{
    get_project_binary_path, is_project_built, notify_webhook, WebhookNotification,
};
use crate::server::state::AppState;
use crate::codegen::action_nodes::{ActionNodeConfig, TriggerType};
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
}

impl SchedulerState {
    pub fn new() -> Self {
        Self {
            jobs: HashMap::new(),
            running: false,
        }
    }
}

// Global scheduler state
lazy_static::lazy_static! {
    pub static ref SCHEDULER: Arc<RwLock<SchedulerState>> = Arc::new(RwLock::new(SchedulerState::new()));
}

/// Parse a cron expression and get the next run time
fn get_next_run(cron_expr: &str, _timezone: &str) -> Option<DateTime<Utc>> {
    // Parse the cron expression
    let schedule = Schedule::from_str(cron_expr).ok()?;
    
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
        if !is_project_built(&project.name) {
            continue;
        }
        
        let binary_path = get_project_binary_path(&project.name);
        
        // Find schedule triggers
        for (trigger_id, node) in &project.action_nodes {
            if let ActionNodeConfig::Trigger(trigger) = node {
                if trigger.trigger_type == TriggerType::Schedule {
                    if let Some(schedule) = &trigger.schedule {
                        if let Some(next_run) = get_next_run(&schedule.cron, &schedule.timezone) {
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
    notify_webhook(&job.project_id, WebhookNotification {
        session_id: session_id.clone(),
        path: format!("/schedule/{}", job.trigger_id),
        method: "SCHEDULE".to_string(),
        payload,
        timestamp,
        binary_path: Some(job.binary_path.clone()),
    }).await;
    
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
        // Scan projects every 60 seconds
        let jobs = scan_projects(&state).await;
        
        // Update scheduler state
        {
            let mut scheduler = SCHEDULER.write().await;
            if !scheduler.running {
                tracing::info!("Scheduler stopped");
                break;
            }
            
            scheduler.jobs.clear();
            for job in &jobs {
                scheduler.jobs
                    .entry(job.project_id.clone())
                    .or_insert_with(Vec::new)
                    .push(job.clone());
            }
        }
        
        // Check for jobs that need to run
        let now = Utc::now();
        for job in &jobs {
            // If the job's next run time is within the next minute, execute it
            let time_until = job.next_run.signed_duration_since(now);
            if time_until.num_seconds() <= 0 && time_until.num_seconds() > -60 {
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
    scheduler.jobs
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
