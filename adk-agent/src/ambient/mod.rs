//! Ambient agents — background agents triggered by event sources.
//!
//! This module provides infrastructure for running agents in the background,
//! triggered by external events like cron schedules, webhooks, or file changes.
//!
//! # Overview
//!
//! - [`EventSource`] — trait for producing trigger events
//! - [`TriggerEvent`] — an event delivered by a source
//! - [`CronTrigger`] — fires on a cron schedule
//! - [`MissedTickPolicy`] — what a schedule does about ticks it was not watching for
//! - [`TickWatermark`] / [`FileTickWatermark`] — records where a schedule left off
//! - [`WebhookTrigger`] — fires on incoming HTTP POST requests
//! - [`FileWatchTrigger`] — fires on filesystem changes matching a glob
//! - [`AmbientAgent`] — wraps an agent + event source with lifecycle control
//! - [`AmbientAgentStatus`] — running/paused/stopped state
//! - [`RunnerTriggerConfig`] / [`TriggerSessionPolicy`] — drive an agent through an
//!   [`AgentInvoker`](adk_core::AgentInvoker) instead of a hand-written handler

/// AmbientAgent lifecycle management.
pub mod agent;
/// CronTrigger event source.
pub mod cron_trigger;
/// Core EventSource trait and TriggerEvent type.
pub mod event_source;
/// FileWatchTrigger event source.
pub mod file_watch_trigger;
/// Driving an agent from a trigger through an AgentInvoker.
pub mod runner_bridge;
/// Durable tick watermarks for CronTrigger.
pub mod watermark;
/// WebhookTrigger event source.
pub mod webhook_trigger;

pub use agent::{AmbientAgent, AmbientAgentStatus, TriggerHandler};
pub use cron_trigger::{CronTrigger, MissedTickPolicy};
pub use event_source::{EventSource, TriggerEvent};
pub use file_watch_trigger::FileWatchTrigger;
pub use runner_bridge::{RunnerTriggerConfig, TriggerSessionPolicy};
pub use watermark::{FileTickWatermark, TickWatermark};
pub use webhook_trigger::{WebhookRequest, WebhookTrigger, WebhookVerifier};
