//! Action Node Type Definitions
//!
//! Contains all struct and enum definitions for action nodes (non-LLM programmatic nodes)
//! in ADK Studio workflows. These types are used by both the code generation module
//! and the runtime execution engine.
//!
//! ## Supported Action Node Types
//!
//! - Trigger: Workflow entry points (webhook, schedule, manual, event)
//! - HTTP: External API calls with authentication
//! - Set: State variable manipulation
//! - Transform: Data transformation using expressions
//! - Switch: Conditional branching
//! - Loop: Iteration over arrays or conditions
//! - Merge: Branch synchronization
//! - Wait: Delays and condition polling
//! - Code: Sandboxed JavaScript execution
//! - Database: SQL/NoSQL operations
//! - Email: Email monitoring and sending
//! - Notification: Slack/Discord/Teams/Webhook notifications
//! - RSS: RSS/Atom feed monitoring
//! - File: Local and cloud file operations

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================
// Action Node Schema Types
// ============================================

/// Error handling mode for action nodes
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorMode {
    #[default]
    Stop,
    Continue,
    Retry,
    Fallback,
}

/// Log level for tracing
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    None,
    #[default]
    Error,
    Info,
    Debug,
}

/// Error handling configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ErrorHandling {
    pub mode: ErrorMode,
    #[serde(default)]
    pub retry_count: Option<u32>,
    #[serde(default)]
    pub retry_delay: Option<u64>,
    #[serde(default)]
    pub fallback_value: Option<serde_json::Value>,
}

/// Tracing configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Tracing {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub log_level: LogLevel,
}

/// Callbacks configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Callbacks {
    #[serde(default)]
    pub on_start: Option<String>,
    #[serde(default)]
    pub on_complete: Option<String>,
    #[serde(default)]
    pub on_error: Option<String>,
}

/// Execution control configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionControl {
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub condition: Option<String>,
}

impl Default for ExecutionControl {
    fn default() -> Self {
        Self { timeout: default_timeout(), condition: None }
    }
}

fn default_timeout() -> u64 {
    30000
}

/// Input/output mapping configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InputOutputMapping {
    #[serde(default)]
    pub input_mapping: Option<HashMap<String, String>>,
    #[serde(default)]
    pub output_key: String,
}

/// Standard properties shared by all action nodes
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StandardProperties {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub error_handling: ErrorHandling,
    #[serde(default)]
    pub tracing: Tracing,
    #[serde(default)]
    pub callbacks: Callbacks,
    #[serde(default)]
    pub execution: ExecutionControl,
    #[serde(default)]
    pub mapping: InputOutputMapping,
}

// ============================================
// Trigger Node Types
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TriggerType {
    Manual,
    Webhook,
    Schedule,
    Event,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookConfig {
    pub path: String,
    pub method: String,
    pub auth: String,
    #[serde(default)]
    pub auth_config: Option<WebhookAuthConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookAuthConfig {
    #[serde(default)]
    pub header_name: Option<String>,
    #[serde(default)]
    pub token_env_var: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleConfig {
    pub cron: String,
    pub timezone: String,
    /// Default prompt/input to send when schedule triggers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventConfig {
    pub source: String,
    pub event_type: String,
    /// Optional JSONPath filter expression
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggerNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub trigger_type: TriggerType,
    #[serde(default)]
    pub webhook: Option<WebhookConfig>,
    #[serde(default)]
    pub schedule: Option<ScheduleConfig>,
    #[serde(default)]
    pub event: Option<EventConfig>,
}

// ============================================
// HTTP Node Types
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpAuth {
    #[serde(rename = "type")]
    pub auth_type: String,
    #[serde(default)]
    pub bearer: Option<BearerAuth>,
    #[serde(default)]
    pub basic: Option<BasicAuth>,
    #[serde(default)]
    pub api_key: Option<ApiKeyAuth>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BearerAuth {
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasicAuth {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyAuth {
    pub header_name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpBody {
    #[serde(rename = "type")]
    pub body_type: String,
    #[serde(default)]
    pub content: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpResponse {
    #[serde(rename = "type")]
    pub response_type: String,
    #[serde(default)]
    pub status_validation: Option<String>,
    #[serde(default)]
    pub json_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimit {
    pub requests_per_window: u32,
    pub window_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub method: HttpMethod,
    pub url: String,
    pub auth: HttpAuth,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    pub body: HttpBody,
    pub response: HttpResponse,
    #[serde(default)]
    pub rate_limit: Option<RateLimit>,
}

// ============================================
// Set Node Types
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SetMode {
    Set,
    Merge,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub key: String,
    pub value: serde_json::Value,
    pub value_type: String,
    #[serde(default)]
    pub is_secret: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvVarsConfig {
    #[serde(default)]
    pub load_from_env: bool,
    #[serde(default)]
    pub prefix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub mode: SetMode,
    #[serde(default)]
    pub variables: Vec<Variable>,
    #[serde(default)]
    pub env_vars: Option<EnvVarsConfig>,
}

// ============================================
// Transform Node Types
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TransformType {
    Jsonpath,
    Jmespath,
    Template,
    Javascript,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinOperation {
    #[serde(rename = "type")]
    pub op_type: String,
    #[serde(default)]
    pub config: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeCoercion {
    pub target_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub transform_type: TransformType,
    #[serde(default)]
    pub expression: String,
    #[serde(default)]
    pub operations: Option<Vec<BuiltinOperation>>,
    #[serde(default)]
    pub type_coercion: Option<TypeCoercion>,
}

// ============================================
// Switch Node Types
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationMode {
    FirstMatch,
    AllMatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchCondition {
    pub id: String,
    pub name: String,
    pub field: String,
    pub operator: String,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    pub output_port: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionMode {
    pub enabled: bool,
    #[serde(default)]
    pub expression: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub evaluation_mode: EvaluationMode,
    #[serde(default)]
    pub conditions: Vec<SwitchCondition>,
    #[serde(default)]
    pub default_branch: Option<String>,
    #[serde(default)]
    pub expression_mode: Option<ExpressionMode>,
}

// ============================================
// Loop Node Types
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LoopType {
    ForEach,
    While,
    Times,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForEachConfig {
    pub source_array: String,
    #[serde(default = "default_item_var")]
    pub item_var: String,
    #[serde(default = "default_index_var")]
    pub index_var: String,
}

fn default_item_var() -> String {
    "item".to_string()
}

fn default_index_var() -> String {
    "index".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WhileConfig {
    pub condition: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimesConfig {
    pub count: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ParallelConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub batch_size: Option<u32>,
    #[serde(default)]
    pub delay_between: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResultsConfig {
    #[serde(default)]
    pub collect: bool,
    #[serde(default)]
    pub aggregation_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub loop_type: LoopType,
    #[serde(default)]
    pub for_each: Option<ForEachConfig>,
    #[serde(rename = "while", default)]
    pub while_config: Option<WhileConfig>,
    #[serde(default)]
    pub times: Option<TimesConfig>,
    #[serde(default)]
    pub parallel: ParallelConfig,
    #[serde(default)]
    pub results: ResultsConfig,
}

// ============================================
// Merge Node Types
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MergeMode {
    WaitAll,
    WaitAny,
    WaitN,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CombineStrategy {
    Array,
    Object,
    First,
    Last,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MergeTimeout {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub ms: u64,
    #[serde(default)]
    pub behavior: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub mode: MergeMode,
    #[serde(default)]
    pub wait_count: Option<u32>,
    pub combine_strategy: CombineStrategy,
    #[serde(default)]
    pub branch_keys: Option<Vec<String>>,
    #[serde(default)]
    pub timeout: MergeTimeout,
}

// ============================================
// Wait Node Types
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WaitType {
    Fixed,
    Until,
    Webhook,
    Condition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixedDuration {
    pub duration: u64,
    pub unit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UntilConfig {
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookWaitConfig {
    pub path: String,
    pub timeout: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConditionPolling {
    pub expression: String,
    pub poll_interval: u64,
    pub max_wait: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub wait_type: WaitType,
    #[serde(default)]
    pub fixed: Option<FixedDuration>,
    #[serde(default)]
    pub until: Option<UntilConfig>,
    #[serde(default)]
    pub webhook: Option<WebhookWaitConfig>,
    #[serde(default)]
    pub condition: Option<ConditionPolling>,
}

// ============================================
// Code Node Types
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CodeLanguage {
    Javascript,
    Typescript,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxConfig {
    #[serde(default)]
    pub network_access: bool,
    #[serde(default)]
    pub file_system_access: bool,
    #[serde(default = "default_memory_limit")]
    pub memory_limit: u32,
    #[serde(default = "default_time_limit")]
    pub time_limit: u64,
}

fn default_memory_limit() -> u32 {
    128
}

fn default_time_limit() -> u64 {
    5000
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            network_access: false,
            file_system_access: false,
            memory_limit: default_memory_limit(),
            time_limit: default_time_limit(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub language: CodeLanguage,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub sandbox: SandboxConfig,
    #[serde(default)]
    pub input_type: Option<String>,
    #[serde(default)]
    pub output_type: Option<String>,
}

// ============================================
// Database Node Types
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseType {
    Postgresql,
    Mysql,
    Sqlite,
    Mongodb,
    Redis,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseConnection {
    pub connection_string: String,
    #[serde(default)]
    pub credential_ref: Option<String>,
    #[serde(default)]
    pub pool_size: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlConfig {
    pub operation: String,
    pub query: String,
    #[serde(default)]
    pub params: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MongoConfig {
    pub collection: String,
    pub operation: String,
    #[serde(default)]
    pub filter: Option<serde_json::Value>,
    #[serde(default)]
    pub document: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RedisConfig {
    pub operation: String,
    pub key: String,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    #[serde(default)]
    pub ttl: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub db_type: DatabaseType,
    pub connection: DatabaseConnection,
    #[serde(default)]
    pub sql: Option<SqlConfig>,
    #[serde(default)]
    pub mongodb: Option<MongoConfig>,
    #[serde(default)]
    pub redis: Option<RedisConfig>,
}

// ============================================
// Email Node Types
// ============================================

/// Email operation mode
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EmailMode {
    Monitor,
    Send,
}

/// IMAP configuration for email monitoring
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImapConfig {
    pub host: String,
    #[serde(default = "default_imap_port")]
    pub port: u16,
    #[serde(default = "default_true")]
    pub secure: bool,
    pub username: String,
    pub password: String,
    #[serde(default = "default_inbox")]
    pub folder: String,
    #[serde(default = "default_true")]
    pub mark_as_read: bool,
}

fn default_imap_port() -> u16 {
    993
}

fn default_inbox() -> String {
    "INBOX".to_string()
}

fn default_true() -> bool {
    true
}

/// Email filter configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EmailFilter {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub date_from: Option<String>,
    #[serde(default)]
    pub date_to: Option<String>,
    #[serde(default = "default_true")]
    pub unread_only: bool,
}

/// SMTP configuration for sending emails
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmtpConfig {
    pub host: String,
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    #[serde(default = "default_true")]
    pub secure: bool,
    pub username: String,
    pub password: String,
    pub from_email: String,
    #[serde(default)]
    pub from_name: Option<String>,
}

fn default_smtp_port() -> u16 {
    587
}

/// Email recipients configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailRecipients {
    pub to: String,
    #[serde(default)]
    pub cc: Option<String>,
    #[serde(default)]
    pub bcc: Option<String>,
}

/// Email body type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EmailBodyType {
    #[default]
    Text,
    Html,
}

/// Email content configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailContent {
    pub subject: String,
    #[serde(default)]
    pub body_type: EmailBodyType,
    pub body: String,
}

/// Email attachment configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailAttachment {
    pub filename: String,
    pub state_key: String,
    #[serde(default)]
    pub mime_type: Option<String>,
}

/// Email node configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub mode: EmailMode,
    #[serde(default)]
    pub imap: Option<ImapConfig>,
    #[serde(default)]
    pub filters: Option<EmailFilter>,
    #[serde(default)]
    pub smtp: Option<SmtpConfig>,
    #[serde(default)]
    pub recipients: Option<EmailRecipients>,
    #[serde(default)]
    pub content: Option<EmailContent>,
    #[serde(default)]
    pub attachments: Option<Vec<EmailAttachment>>,
}

// ============================================
// Notification Node Types
// ============================================

/// Notification channel type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationChannel {
    Slack,
    Discord,
    Teams,
    Webhook,
}

/// Message format type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MessageFormat {
    #[default]
    Plain,
    Markdown,
    Blocks,
}

/// Notification message configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationMessage {
    /// Message text (supports {{variable}} interpolation)
    pub text: String,
    /// Message format
    #[serde(default)]
    pub format: MessageFormat,
    /// Block Kit (Slack) / Embeds (Discord) / Adaptive Cards (Teams)
    #[serde(default)]
    pub blocks: Option<Vec<serde_json::Value>>,
}

/// Notification node configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    /// Notification channel
    pub channel: NotificationChannel,
    /// Webhook URL (marked as secret)
    pub webhook_url: String,
    /// Message configuration
    pub message: NotificationMessage,
    /// Custom username for the notification
    #[serde(default)]
    pub username: Option<String>,
    /// Custom icon URL for the notification
    #[serde(default)]
    pub icon_url: Option<String>,
    /// Target channel (for Slack)
    #[serde(default)]
    pub target_channel: Option<String>,
}

// ============================================
// RSS/Feed Node Types
// ============================================

/// Feed filter configuration for RSS monitoring
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FeedFilter {
    /// Filter by keywords in title or description
    #[serde(default)]
    pub keywords: Option<Vec<String>>,
    /// Filter by author
    #[serde(default)]
    pub author: Option<String>,
    /// Filter by date - only entries after this date
    #[serde(default)]
    pub date_from: Option<String>,
    /// Filter by date - only entries before this date
    #[serde(default)]
    pub date_to: Option<String>,
    /// Filter by category/tag
    #[serde(default)]
    pub categories: Option<Vec<String>>,
}

/// Seen item tracking configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeenItemTracking {
    /// Enable tracking of seen items to avoid duplicates
    #[serde(default)]
    pub enabled: bool,
    /// State key to store seen item IDs
    #[serde(default)]
    pub state_key: String,
    /// Maximum number of seen items to track
    #[serde(default = "default_max_seen_items")]
    pub max_items: u32,
}

fn default_max_seen_items() -> u32 {
    1000
}

impl Default for SeenItemTracking {
    fn default() -> Self {
        Self {
            enabled: false,
            state_key: "seen_feed_items".to_string(),
            max_items: default_max_seen_items(),
        }
    }
}

/// RSS/Feed node configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RssNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    /// Feed URL to monitor
    pub feed_url: String,
    /// Poll interval in milliseconds
    #[serde(default = "default_poll_interval")]
    pub poll_interval: u64,
    /// Feed filters
    #[serde(default)]
    pub filters: Option<FeedFilter>,
    /// Seen item tracking configuration
    #[serde(default)]
    pub seen_tracking: Option<SeenItemTracking>,
    /// Maximum number of entries to return per poll
    #[serde(default)]
    pub max_entries: Option<u32>,
    /// Include full content or just summary
    #[serde(default)]
    pub include_content: bool,
    /// Parse media/enclosures
    #[serde(default)]
    pub parse_media: bool,
}

fn default_poll_interval() -> u64 {
    300000 // 5 minutes
}

// ============================================
// File Node Types
// ============================================

/// File operation type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FileOperation {
    Read,
    Write,
    Delete,
    List,
}

/// File format for parsing
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FileFormat {
    Json,
    Csv,
    Xml,
    #[default]
    Text,
    Binary,
}

/// Cloud storage provider
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CloudProvider {
    S3,
    Gcs,
    Azure,
}

/// Local file configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalFileConfig {
    /// File path (supports {{variable}} interpolation)
    pub path: String,
    /// File encoding (default: utf-8)
    #[serde(default = "default_encoding")]
    pub encoding: String,
}

fn default_encoding() -> String {
    "utf-8".to_string()
}

/// CSV parsing options
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CsvOptions {
    /// Field delimiter (default: ,)
    #[serde(default = "default_csv_delimiter")]
    pub delimiter: String,
    /// First row contains headers
    #[serde(default)]
    pub has_header: bool,
    /// Quote character (default: ")
    #[serde(default = "default_quote_char")]
    pub quote_char: String,
    /// Escape character
    #[serde(default)]
    pub escape_char: Option<String>,
}

fn default_csv_delimiter() -> String {
    ",".to_string()
}

fn default_quote_char() -> String {
    "\"".to_string()
}

impl Default for CsvOptions {
    fn default() -> Self {
        Self {
            delimiter: default_csv_delimiter(),
            has_header: true,
            quote_char: default_quote_char(),
            escape_char: None,
        }
    }
}

/// File parsing configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileParseConfig {
    /// File format to parse
    pub format: FileFormat,
    /// CSV-specific options
    #[serde(default)]
    pub csv_options: Option<CsvOptions>,
    /// XML root element to extract
    #[serde(default)]
    pub xml_root_element: Option<String>,
}

/// Cloud storage configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudStorageConfig {
    /// Cloud provider
    pub provider: CloudProvider,
    /// Bucket/container name
    pub bucket: String,
    /// Object key/path
    pub key: String,
    /// Credentials reference (state key or Set node reference)
    pub credentials: String,
    /// AWS region (for S3)
    #[serde(default)]
    pub region: Option<String>,
    /// Generate presigned URL
    #[serde(default)]
    pub presigned_url: bool,
    /// Presigned URL expiration in seconds
    #[serde(default = "default_presigned_expiry")]
    pub presigned_expiry: u64,
}

fn default_presigned_expiry() -> u64 {
    3600 // 1 hour
}

/// File write configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileWriteConfig {
    /// Content to write (state key or expression)
    pub content: String,
    /// Create parent directories if they don't exist
    #[serde(default)]
    pub create_dirs: bool,
    /// Append to file instead of overwrite
    #[serde(default)]
    pub append: bool,
}

/// File list configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileListConfig {
    /// Directory path to list
    pub path: String,
    /// Include subdirectories recursively
    #[serde(default)]
    pub recursive: bool,
    /// File pattern filter (glob)
    #[serde(default)]
    pub pattern: Option<String>,
}

/// File node configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    /// File operation type
    pub operation: FileOperation,
    /// Local file configuration
    #[serde(default)]
    pub local: Option<LocalFileConfig>,
    /// Cloud storage configuration
    #[serde(default)]
    pub cloud: Option<CloudStorageConfig>,
    /// File parsing options (for read operation)
    #[serde(default)]
    pub parse: Option<FileParseConfig>,
    /// File write options (for write operation)
    #[serde(default)]
    pub write: Option<FileWriteConfig>,
    /// File list options (for list operation)
    #[serde(default)]
    pub list: Option<FileListConfig>,
}

// ============================================
// Action Node Union Type
// ============================================

/// Union type for all action node configurations
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionNodeConfig {
    Trigger(TriggerNodeConfig),
    Http(HttpNodeConfig),
    Set(SetNodeConfig),
    Transform(TransformNodeConfig),
    Switch(SwitchNodeConfig),
    Loop(LoopNodeConfig),
    Merge(MergeNodeConfig),
    Wait(WaitNodeConfig),
    Code(CodeNodeConfig),
    Database(DatabaseNodeConfig),
    Email(EmailNodeConfig),
    Notification(NotificationNodeConfig),
    Rss(RssNodeConfig),
    File(FileNodeConfig),
}

impl ActionNodeConfig {
    /// Get the standard properties for any action node
    pub fn standard(&self) -> &StandardProperties {
        match self {
            ActionNodeConfig::Trigger(n) => &n.standard,
            ActionNodeConfig::Http(n) => &n.standard,
            ActionNodeConfig::Set(n) => &n.standard,
            ActionNodeConfig::Transform(n) => &n.standard,
            ActionNodeConfig::Switch(n) => &n.standard,
            ActionNodeConfig::Loop(n) => &n.standard,
            ActionNodeConfig::Merge(n) => &n.standard,
            ActionNodeConfig::Wait(n) => &n.standard,
            ActionNodeConfig::Code(n) => &n.standard,
            ActionNodeConfig::Database(n) => &n.standard,
            ActionNodeConfig::Email(n) => &n.standard,
            ActionNodeConfig::Notification(n) => &n.standard,
            ActionNodeConfig::Rss(n) => &n.standard,
            ActionNodeConfig::File(n) => &n.standard,
        }
    }

    /// Get the node type as a string
    pub fn node_type(&self) -> &'static str {
        match self {
            ActionNodeConfig::Trigger(_) => "trigger",
            ActionNodeConfig::Http(_) => "http",
            ActionNodeConfig::Set(_) => "set",
            ActionNodeConfig::Transform(_) => "transform",
            ActionNodeConfig::Switch(_) => "switch",
            ActionNodeConfig::Loop(_) => "loop",
            ActionNodeConfig::Merge(_) => "merge",
            ActionNodeConfig::Wait(_) => "wait",
            ActionNodeConfig::Code(_) => "code",
            ActionNodeConfig::Database(_) => "database",
            ActionNodeConfig::Email(_) => "email",
            ActionNodeConfig::Notification(_) => "notification",
            ActionNodeConfig::Rss(_) => "rss",
            ActionNodeConfig::File(_) => "file",
        }
    }

    /// Return the state keys this action node is expected to produce.
    ///
    /// Used by the SSE backend to reconstruct per-node output snapshots
    /// when the generated binary doesn't emit intermediate state updates.
    pub fn expected_output_keys(&self) -> Vec<String> {
        match self {
            ActionNodeConfig::Set(cfg) => {
                // Set nodes produce one key per variable
                cfg.variables.iter().map(|v| v.key.clone()).collect()
            }
            ActionNodeConfig::Transform(cfg) => {
                // Transform nodes produce a single output key
                let key = &cfg.standard.mapping.output_key;
                if key.is_empty() { vec![] } else { vec![key.clone()] }
            }
            ActionNodeConfig::Switch(cfg) => {
                // Switch nodes produce a branch key
                let key = &cfg.standard.mapping.output_key;
                if key.is_empty() { vec!["branch".to_string()] } else { vec![key.clone()] }
            }
            ActionNodeConfig::Loop(cfg) => {
                // Loop nodes produce iteration variables and optionally aggregated results
                let id = &cfg.standard.id;
                let mut keys = vec![
                    format!("{}_loop_index", id),
                    format!("{}_loop_done", id),
                    "index".to_string(),
                ];
                match cfg.loop_type {
                    LoopType::ForEach => {
                        let item_var = cfg
                            .for_each
                            .as_ref()
                            .map(|f| f.item_var.clone())
                            .unwrap_or_else(|| "item".to_string());
                        let index_var = cfg
                            .for_each
                            .as_ref()
                            .map(|f| f.index_var.clone())
                            .unwrap_or_else(|| "index".to_string());
                        keys.push(item_var);
                        if index_var != "index" {
                            keys.push(index_var);
                        }
                    }
                    _ => {}
                }
                if cfg.results.collect {
                    let agg_key = cfg.results.aggregation_key.as_deref().unwrap_or("loop_results");
                    keys.push(agg_key.to_string());
                }
                keys
            }
            ActionNodeConfig::Merge(cfg) => {
                // Merge nodes produce a single merged output key
                let key = &cfg.standard.mapping.output_key;
                if key.is_empty() { vec!["merged".to_string()] } else { vec![key.clone()] }
            }
            _ => {
                // Other nodes use the standard output_key mapping
                let key = &self.standard().mapping.output_key;
                if key.is_empty() { vec![] } else { vec![key.clone()] }
            }
        }
    }
}

// ============================================
// Action Error Types
// ============================================

/// Errors that can occur during action node execution
#[derive(Debug, Clone)]
pub enum ActionError {
    /// HTTP request returned unexpected status
    HttpStatus { status: u16, expected: String },
    /// Timeout during node execution
    Timeout { node: String, timeout_ms: u64 },
    /// No matching branch in switch node
    NoMatchingBranch { node: String },
    /// Transform operation failed
    Transform(String),
    /// Code execution failed
    CodeExecution(String),
    /// Sandbox initialization failed
    SandboxInit(String),
    /// Missing credential reference
    MissingCredential(String),
    /// No database specified
    NoDatabase,
    /// Invalid timestamp format
    InvalidTimestamp(String),
    /// Webhook wait timed out
    WebhookTimeout,
    /// Webhook was cancelled
    WebhookCancelled,
    /// Condition polling timed out
    ConditionTimeout { condition: String, timeout_ms: u64 },
    /// No branch completed in merge
    NoBranchCompleted,
    /// Insufficient branches completed
    InsufficientBranches { expected: u32, got: usize },
    /// Email authentication failed
    EmailAuth(String),
    /// Email send failed
    EmailSend(String),
    /// Notification send failed
    NotificationSend(String),
    /// RSS feed fetch failed
    RssFetch(String),
    /// RSS feed parse failed
    RssParse(String),
    /// File read failed
    FileRead(String),
    /// File write failed
    FileWrite(String),
    /// File delete failed
    FileDelete(String),
    /// File parse failed
    FileParse(String),
    /// Generic error
    Other(String),
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionError::HttpStatus { status, expected } => {
                write!(f, "HTTP status {} not in expected range {}", status, expected)
            }
            ActionError::Timeout { node, timeout_ms } => {
                write!(f, "Node '{}' timed out after {}ms", node, timeout_ms)
            }
            ActionError::NoMatchingBranch { node } => {
                write!(f, "No matching branch in switch node '{}'", node)
            }
            ActionError::Transform(msg) => write!(f, "Transform error: {}", msg),
            ActionError::CodeExecution(msg) => write!(f, "Code execution error: {}", msg),
            ActionError::SandboxInit(msg) => write!(f, "Sandbox init error: {}", msg),
            ActionError::MissingCredential(name) => {
                write!(f, "Missing credential: {}", name)
            }
            ActionError::NoDatabase => write!(f, "No database specified in connection string"),
            ActionError::InvalidTimestamp(msg) => write!(f, "Invalid timestamp: {}", msg),
            ActionError::WebhookTimeout => write!(f, "Webhook wait timed out"),
            ActionError::WebhookCancelled => write!(f, "Webhook was cancelled"),
            ActionError::ConditionTimeout { condition, timeout_ms } => {
                write!(f, "Condition '{}' not met within {}ms", condition, timeout_ms)
            }
            ActionError::NoBranchCompleted => write!(f, "No branch completed in merge"),
            ActionError::InsufficientBranches { expected, got } => {
                write!(f, "Expected {} branches, got {}", expected, got)
            }
            ActionError::EmailAuth(msg) => write!(f, "Email authentication failed: {}", msg),
            ActionError::EmailSend(msg) => write!(f, "Email send failed: {}", msg),
            ActionError::NotificationSend(msg) => write!(f, "Notification send failed: {}", msg),
            ActionError::RssFetch(msg) => write!(f, "RSS feed fetch failed: {}", msg),
            ActionError::RssParse(msg) => write!(f, "RSS feed parse failed: {}", msg),
            ActionError::FileRead(msg) => write!(f, "File read failed: {}", msg),
            ActionError::FileWrite(msg) => write!(f, "File write failed: {}", msg),
            ActionError::FileDelete(msg) => write!(f, "File delete failed: {}", msg),
            ActionError::FileParse(msg) => write!(f, "File parse failed: {}", msg),
            ActionError::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for ActionError {}
