//! Action node type definitions for all 14 node types.
//!
//! These types are the shared contract between `adk-studio` (visual builder)
//! and `adk-graph` (runtime engine).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Shared enums ──────────────────────────────────────────────────────

/// Error handling mode for action nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorMode {
    Stop,
    Continue,
    Retry,
    Fallback,
}

/// Log level for action node tracing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    None,
    Error,
    Info,
    Debug,
}

/// Trigger type for trigger nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerType {
    Manual,
    Webhook,
    Schedule,
    Event,
}

/// HTTP method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

/// Set node mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetMode {
    Set,
    Merge,
    Delete,
}

/// Transform type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransformType {
    Jsonpath,
    Template,
    Builtin,
}

/// Switch condition evaluation mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationMode {
    FirstMatch,
    AllMatch,
}

/// Loop type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopType {
    ForEach,
    While,
    Times,
}

/// Merge mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMode {
    WaitAll,
    WaitAny,
    WaitN,
}

/// Merge combine strategy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombineStrategy {
    Array,
    Object,
    First,
    Last,
}

/// Wait type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitType {
    Fixed,
    Until,
    Condition,
    Webhook,
}

/// Code language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeLanguage {
    Rust,
    Javascript,
    Typescript,
}

/// Database type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseType {
    Postgresql,
    Mysql,
    Sqlite,
    Mongodb,
    Redis,
}

/// Email mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailMode {
    Monitor,
    Send,
}

/// Email body type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailBodyType {
    Text,
    Html,
}

/// Notification channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationChannel {
    Slack,
    Discord,
    Teams,
    Webhook,
}

/// Message format for notifications.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageFormat {
    Text,
    Markdown,
    Html,
}

/// File operation type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOperation {
    Read,
    Write,
    Delete,
    List,
}

/// File format for parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileFormat {
    Json,
    Csv,
    Xml,
    Text,
    Binary,
}

/// Cloud storage provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudProvider {
    Aws,
    Gcp,
    Azure,
}

// ── Shared structs ────────────────────────────────────────────────────

/// Error handling configuration for an action node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorHandling {
    pub mode: ErrorMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_delay: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_value: Option<serde_json::Value>,
}

/// Tracing configuration for an action node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tracing {
    pub enabled: bool,
    pub log_level: LogLevel,
}

/// Callback hooks for an action node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Callbacks {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_complete: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_error: Option<String>,
}

/// Execution control for an action node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionControl {
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

fn default_timeout() -> u64 {
    30000
}

/// Input/output mapping for an action node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputOutputMapping {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_mapping: Option<HashMap<String, String>>,
    pub output_key: String,
}

/// Standard properties shared by every action node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StandardProperties {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<(f64, f64)>,
    pub error_handling: ErrorHandling,
    pub tracing: Tracing,
    pub callbacks: Callbacks,
    pub execution: ExecutionControl,
    pub mapping: InputOutputMapping,
}

// ── Trigger node types ────────────────────────────────────────────────

/// Manual trigger configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualTriggerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_prompt: Option<String>,
}

/// Webhook authentication configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookAuthConfig {
    #[serde(rename = "type")]
    pub auth_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

/// Webhook trigger configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookConfig {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<HttpMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<WebhookAuthConfig>,
}

/// Schedule trigger configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleConfig {
    pub cron: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_prompt: Option<String>,
}

/// Event trigger configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventConfig {
    pub source: String,
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

/// Trigger node configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggerNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub trigger_type: TriggerType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual: Option<ManualTriggerConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook: Option<WebhookConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<ScheduleConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<EventConfig>,
}

// ── HTTP node types ───────────────────────────────────────────────────

/// Bearer token authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BearerAuth {
    pub token: String,
}

/// Basic authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BasicAuth {
    pub username: String,
    pub password: String,
}

/// API key authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyAuth {
    pub header: String,
    pub value: String,
}

/// HTTP authentication configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HttpAuth {
    None,
    Bearer(BearerAuth),
    Basic(BasicAuth),
    ApiKey(ApiKeyAuth),
}

/// HTTP request body configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HttpBody {
    None,
    Json { data: serde_json::Value },
    Form { fields: HashMap<String, String> },
    Raw { content: String, content_type: String },
}

/// HTTP response handling configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpResponse {
    #[serde(rename = "type")]
    pub response_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_validation: Option<String>,
}

/// Rate limiting configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimit {
    pub max_requests: u32,
    pub window_ms: u64,
}

/// HTTP action node configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimit>,
}

// ── Set node types ────────────────────────────────────────────────────

/// A variable to set in state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub key: String,
    pub value: serde_json::Value,
    #[serde(default)]
    pub value_type: String,
    #[serde(default)]
    pub is_secret: bool,
}

/// Environment variable loading configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvVarsConfig {
    pub load_from_env: bool,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub keys: Vec<String>,
}

/// Set action node configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub mode: SetMode,
    pub variables: Vec<Variable>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_vars: Option<EnvVarsConfig>,
}

// ── Transform node types ──────────────────────────────────────────────

/// Built-in transform operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinOperation {
    pub operation: String,
    #[serde(default)]
    pub params: HashMap<String, String>,
}

/// Type coercion configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeCoercion {
    pub from_type: String,
    pub to_type: String,
}

/// Transform action node configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub transform_type: TransformType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin: Option<BuiltinOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coercion: Option<TypeCoercion>,
}

// ── Switch node types ─────────────────────────────────────────────────

/// Expression mode for switch conditions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionMode {
    pub field: String,
    pub operator: String,
    pub value: String,
}

/// A single switch condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchCondition {
    pub id: String,
    pub name: String,
    pub expression: ExpressionMode,
    pub output_port: String,
}

/// Switch action node configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub evaluation_mode: EvaluationMode,
    pub conditions: Vec<SwitchCondition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
}

// ── Loop node types ───────────────────────────────────────────────────

/// ForEach loop configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForEachConfig {
    pub source: String,
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

/// While loop configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WhileConfig {
    pub condition: String,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
}

fn default_max_iterations() -> u32 {
    1000
}

/// Times loop configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimesConfig {
    pub count: u32,
    #[serde(default = "default_index_var")]
    pub index_var: String,
}

/// Parallel execution configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParallelConfig {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_between: Option<u64>,
}

/// Results collection configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultsConfig {
    pub collect: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregation_key: Option<String>,
}

/// Loop action node configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub loop_type: LoopType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub for_each: Option<ForEachConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub while_config: Option<WhileConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub times: Option<TimesConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel: Option<ParallelConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<ResultsConfig>,
}

// ── Merge node types ──────────────────────────────────────────────────

/// Merge timeout configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeTimeout {
    pub timeout_ms: u64,
    #[serde(default)]
    pub on_timeout: String,
}

/// Merge action node configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub mode: MergeMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_count: Option<u32>,
    pub combine_strategy: CombineStrategy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<MergeTimeout>,
}

// ── Wait node types ───────────────────────────────────────────────────

/// Fixed duration wait configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixedDuration {
    pub duration: u64,
    #[serde(default = "default_duration_unit")]
    pub unit: String,
}

fn default_duration_unit() -> String {
    "ms".to_string()
}

/// Wait-until configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UntilConfig {
    pub timestamp: String,
}

/// Webhook wait configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookWaitConfig {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Condition polling configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConditionPolling {
    pub condition: String,
    pub interval_ms: u64,
    pub max_wait_ms: u64,
}

/// Wait action node configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub wait_type: WaitType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed: Option<FixedDuration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<UntilConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook: Option<WebhookWaitConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<ConditionPolling>,
}

// ── Code node types ───────────────────────────────────────────────────

/// Sandbox resource limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit_mb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_limit_ms: Option<u64>,
    #[serde(default)]
    pub allow_network: bool,
    #[serde(default)]
    pub allow_fs: bool,
}

/// Code action node configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub language: CodeLanguage,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxConfig>,
}

// ── Database node types ───────────────────────────────────────────────

/// Database connection configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseConnection {
    pub database_type: DatabaseType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_string: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
}

/// SQL query configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlConfig {
    pub query: String,
    #[serde(default)]
    pub params: Vec<serde_json::Value>,
    #[serde(default = "default_sql_operation")]
    pub operation: String,
}

fn default_sql_operation() -> String {
    "query".to_string()
}

/// MongoDB operation configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MongoConfig {
    pub collection: String,
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<serde_json::Value>,
}

/// Redis command configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RedisConfig {
    pub command: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<u64>,
}

/// Database action node configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub connection: DatabaseConnection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sql: Option<SqlConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mongo: Option<MongoConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redis: Option<RedisConfig>,
}

// ── Email node types ──────────────────────────────────────────────────

/// IMAP connection configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImapConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    #[serde(default)]
    pub tls: bool,
}

/// Email filter configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(default)]
    pub unread_only: bool,
    #[serde(default)]
    pub mark_as_read: bool,
}

/// SMTP connection configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    #[serde(default)]
    pub tls: bool,
}

/// Email recipients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailRecipients {
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub bcc: Vec<String>,
}

/// Email content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailContent {
    pub subject: String,
    pub body: String,
    pub body_type: EmailBodyType,
}

/// Email attachment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailAttachment {
    pub filename: String,
    pub state_key: String,
}

/// Email action node configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub mode: EmailMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imap: Option<ImapConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<EmailFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smtp: Option<SmtpConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipients: Option<EmailRecipients>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<EmailContent>,
    #[serde(default)]
    pub attachments: Vec<EmailAttachment>,
}

// ── Notification node types ───────────────────────────────────────────

/// Notification message configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationMessage {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<MessageFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

/// Notification action node configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub notification_channel: NotificationChannel,
    pub webhook_url: String,
    pub message: NotificationMessage,
}

// ── RSS node types ────────────────────────────────────────────────────

/// Feed filter configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedFilter {
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
}

/// Seen item tracking configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeenItemTracking {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u32>,
}

/// RSS action node configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RssNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub feed_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<FeedFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_tracking: Option<SeenItemTracking>,
}

// ── File node types ───────────────────────────────────────────────────

/// Local file configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalFileConfig {
    pub path: String,
}

/// Cloud storage configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudStorageConfig {
    pub provider: CloudProvider,
    pub bucket: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
}

/// File parse configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileParseConfig {
    pub format: FileFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub csv_options: Option<CsvOptions>,
}

/// CSV parsing options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CsvOptions {
    #[serde(default = "default_csv_delimiter")]
    pub delimiter: String,
    #[serde(default = "default_true")]
    pub has_header: bool,
}

fn default_csv_delimiter() -> String {
    ",".to_string()
}

fn default_true() -> bool {
    true
}

/// File write configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileWriteConfig {
    pub content: serde_json::Value,
    #[serde(default)]
    pub create_dirs: bool,
    #[serde(default)]
    pub append: bool,
}

/// File list configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileListConfig {
    #[serde(default)]
    pub recursive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

/// File action node configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileNodeConfig {
    #[serde(flatten)]
    pub standard: StandardProperties,
    pub operation: FileOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalFileConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud: Option<CloudStorageConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parse: Option<FileParseConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write: Option<FileWriteConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<FileListConfig>,
}

// ── ActionNodeConfig tagged union ─────────────────────────────────────

/// Tagged union of all 14 action node types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Returns a reference to the standard properties shared by all node types.
    pub fn standard(&self) -> &StandardProperties {
        match self {
            Self::Trigger(c) => &c.standard,
            Self::Http(c) => &c.standard,
            Self::Set(c) => &c.standard,
            Self::Transform(c) => &c.standard,
            Self::Switch(c) => &c.standard,
            Self::Loop(c) => &c.standard,
            Self::Merge(c) => &c.standard,
            Self::Wait(c) => &c.standard,
            Self::Code(c) => &c.standard,
            Self::Database(c) => &c.standard,
            Self::Email(c) => &c.standard,
            Self::Notification(c) => &c.standard,
            Self::Rss(c) => &c.standard,
            Self::File(c) => &c.standard,
        }
    }

    /// Returns the node type as a string.
    pub fn node_type(&self) -> &'static str {
        match self {
            Self::Trigger(_) => "trigger",
            Self::Http(_) => "http",
            Self::Set(_) => "set",
            Self::Transform(_) => "transform",
            Self::Switch(_) => "switch",
            Self::Loop(_) => "loop",
            Self::Merge(_) => "merge",
            Self::Wait(_) => "wait",
            Self::Code(_) => "code",
            Self::Database(_) => "database",
            Self::Email(_) => "email",
            Self::Notification(_) => "notification",
            Self::Rss(_) => "rss",
            Self::File(_) => "file",
        }
    }

    /// Returns the expected output keys for this node type.
    pub fn expected_output_keys(&self) -> Vec<String> {
        let output_key = self.standard().mapping.output_key.clone();
        match self {
            Self::Switch(c) => {
                let mut keys: Vec<String> =
                    c.conditions.iter().map(|cond| cond.output_port.clone()).collect();
                if let Some(default) = &c.default_branch {
                    keys.push(default.clone());
                }
                keys
            }
            _ => vec![output_key],
        }
    }
}
