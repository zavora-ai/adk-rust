/**
 * Action Node Type Definitions
 * 
 * Defines all 10 action node configuration interfaces for non-LLM
 * programmatic nodes in ADK Studio workflows.
 * 
 * @see Requirements 2-11
 */

import type { StandardProperties } from './standardProperties';

// ============================================
// 1. TRIGGER NODE (Requirement 2)
// ============================================

/**
 * Trigger type for workflow entry points.
 * @see Requirement 2.1
 */
export type TriggerType = 'manual' | 'webhook' | 'schedule' | 'event';

/**
 * Manual trigger configuration for user input.
 * Defines the label and placeholder text shown in the chat input
 * when a manual trigger workflow is ready to execute.
 * @see trigger-input-flow Requirements 1.1, 1.2
 */
export interface ManualTriggerConfig {
  /** Label shown above input field (e.g., "Enter your question") */
  inputLabel: string;
  /** Placeholder text for input field (e.g., "Type a message to start...") */
  defaultPrompt: string;
}

/**
 * Default values for manual trigger configuration.
 * @see trigger-input-flow Requirements 1.1, 1.2
 */
export const DEFAULT_MANUAL_TRIGGER_CONFIG: ManualTriggerConfig = {
  inputLabel: 'Enter your message',
  defaultPrompt: 'What can you help me build with ADK-Rust today?',
};

/**
 * HTTP method for webhook triggers.
 */
export type WebhookMethod = 'GET' | 'POST';

/**
 * Authentication type for webhooks.
 */
export type WebhookAuth = 'none' | 'bearer' | 'api_key';

/**
 * Webhook configuration.
 * @see Requirement 2.2
 */
export interface WebhookConfig {
  /** Webhook path (e.g., '/api/webhook/my-flow') */
  path: string;
  /** HTTP method */
  method: WebhookMethod;
  /** Authentication type */
  auth: WebhookAuth;
  /** Authentication configuration */
  authConfig?: {
    /** Header name for API key auth */
    headerName?: string;
    /** Environment variable for token */
    tokenEnvVar?: string;
  };
}

/**
 * Schedule configuration with cron expression.
 * @see Requirement 2.3
 */
export interface ScheduleConfig {
  /** Cron expression */
  cron: string;
  /** Timezone (e.g., 'America/New_York') */
  timezone: string;
  /** Default prompt/input to send when schedule triggers */
  defaultPrompt?: string;
}

/**
 * Event trigger configuration.
 */
export interface EventConfig {
  /** Event source identifier */
  source: string;
  /** Event type to listen for */
  eventType: string;
  /** Optional filter expression (JSONPath) to filter events */
  filter?: string;
}

/**
 * Trigger node configuration.
 * Entry point for workflow execution.
 * @see Requirement 2
 */
export interface TriggerNodeConfig extends StandardProperties {
  type: 'trigger';
  triggerType: TriggerType;
  webhook?: WebhookConfig;
  schedule?: ScheduleConfig;
  event?: EventConfig;
  /** Manual trigger specific configuration (input label and default prompt) */
  manual?: ManualTriggerConfig;
}

// ============================================
// 2. HTTP NODE (Requirement 3)
// ============================================

/**
 * HTTP methods supported by HTTP node.
 * @see Requirement 3.1
 */
export type HttpMethod = 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE';

/**
 * Authentication type for HTTP requests.
 * @see Requirement 3.2
 */
export type HttpAuthType = 'none' | 'bearer' | 'basic' | 'api_key';

/**
 * HTTP authentication configuration.
 * @see Requirement 3.2
 */
export interface HttpAuth {
  type: HttpAuthType;
  bearer?: { token: string };
  basic?: { username: string; password: string };
  apiKey?: { headerName: string; value: string };
}

/**
 * HTTP body type.
 * @see Requirement 3.3
 */
export type HttpBodyType = 'none' | 'json' | 'form' | 'raw';

/**
 * HTTP body configuration.
 * @see Requirement 3.3
 */
export interface HttpBody {
  type: HttpBodyType;
  content?: string | Record<string, unknown>;
}

/**
 * HTTP response type.
 * @see Requirement 3.4
 */
export type HttpResponseType = 'json' | 'text' | 'binary';

/**
 * HTTP response handling configuration.
 * @see Requirement 3.4
 */
export interface HttpResponse {
  type: HttpResponseType;
  /** Status code validation (e.g., '200-299') */
  statusValidation?: string;
  /** JSONPath expression to extract specific field */
  jsonPath?: string;
}

/**
 * Rate limiting configuration.
 * @see Requirement 3.5
 */
export interface RateLimit {
  /** Maximum requests per time window */
  requestsPerWindow: number;
  /** Time window in milliseconds */
  windowMs: number;
}

/**
 * HTTP node configuration.
 * Makes HTTP requests to external APIs.
 * @see Requirement 3
 */
export interface HttpNodeConfig extends StandardProperties {
  type: 'http';
  method: HttpMethod;
  /** URL with variable interpolation support ({{variable}}) */
  url: string;
  auth: HttpAuth;
  headers: Record<string, string>;
  body: HttpBody;
  response: HttpResponse;
  rateLimit?: RateLimit;
}

// ============================================
// 3. SET NODE (Requirement 4)
// ============================================

/**
 * Variable operation mode.
 * @see Requirement 4.2
 */
export type SetMode = 'set' | 'merge' | 'delete';

/**
 * Variable value type.
 * @see Requirement 4.1
 */
export type VariableValueType = 'string' | 'number' | 'boolean' | 'json' | 'expression';

/**
 * Variable definition.
 * @see Requirement 4.1
 */
export interface Variable {
  key: string;
  value: string | number | boolean | object;
  valueType: VariableValueType;
  /** Mark as secret (masked in logs) */
  isSecret: boolean;
}

/**
 * Environment variable loading configuration.
 * @see Requirement 4.3
 */
export interface EnvVarsConfig {
  /** Load from .env file */
  loadFromEnv: boolean;
  /** Filter by prefix */
  prefix?: string;
}

/**
 * Set node configuration.
 * Defines and manipulates workflow state variables.
 * @see Requirement 4
 */
export interface SetNodeConfig extends StandardProperties {
  type: 'set';
  mode: SetMode;
  variables: Variable[];
  envVars?: EnvVarsConfig;
}

// ============================================
// 4. TRANSFORM NODE (Requirement 5)
// ============================================

/**
 * Transform type.
 * @see Requirement 5.1
 */
export type TransformType = 'jsonpath' | 'jmespath' | 'template' | 'javascript';

/**
 * Built-in operation type.
 * @see Requirement 5.2
 */
export type BuiltinOperationType = 'pick' | 'omit' | 'rename' | 'flatten' | 'sort' | 'unique';

/**
 * Built-in operation configuration.
 * @see Requirement 5.2
 */
export interface BuiltinOperation {
  type: BuiltinOperationType;
  config: Record<string, unknown>;
}

/**
 * Type coercion target type.
 * @see Requirement 5.3
 */
export type CoercionTargetType = 'string' | 'number' | 'boolean' | 'array' | 'object';

/**
 * Type coercion configuration.
 * @see Requirement 5.3
 */
export interface TypeCoercion {
  targetType: CoercionTargetType;
}

/**
 * Transform node configuration.
 * Transforms data using expressions or built-in operations.
 * @see Requirement 5
 */
export interface TransformNodeConfig extends StandardProperties {
  type: 'transform';
  transformType: TransformType;
  /** Transformation expression */
  expression: string;
  /** Built-in operations (alternative to expression) */
  operations?: BuiltinOperation[];
  typeCoercion?: TypeCoercion;
}

// ============================================
// 5. SWITCH NODE (Requirement 6)
// ============================================

/**
 * Condition operator.
 * @see Requirement 6.1
 */
export type ConditionOperator =
  | 'eq' | 'neq' | 'gt' | 'lt' | 'gte' | 'lte'
  | 'contains' | 'startsWith' | 'endsWith'
  | 'matches' | 'in' | 'empty' | 'exists';

/**
 * Switch condition definition.
 * @see Requirement 6.1
 */
export interface SwitchCondition {
  id: string;
  name: string;
  field: string;
  operator: ConditionOperator;
  value?: unknown;
  /** Output port to use when condition matches */
  outputPort: string;
}

/**
 * Evaluation mode for switch node.
 * @see Requirement 6.2
 */
export type EvaluationMode = 'first_match' | 'all_match';

/**
 * Expression mode configuration.
 * @see Requirement 6.3
 */
export interface ExpressionMode {
  enabled: boolean;
  /** JavaScript expression returning branch name */
  expression: string;
}

/**
 * Switch node configuration.
 * Conditional branching based on conditions or expressions.
 * @see Requirement 6
 */
export interface SwitchNodeConfig extends StandardProperties {
  type: 'switch';
  evaluationMode: EvaluationMode;
  conditions: SwitchCondition[];
  /** Output port for no match */
  defaultBranch?: string;
  expressionMode?: ExpressionMode;
}

// ============================================
// 6. LOOP NODE (Requirement 7)
// ============================================

/**
 * Loop type.
 * @see Requirement 7.1
 */
export type LoopType = 'forEach' | 'while' | 'times';

/**
 * forEach loop configuration.
 * @see Requirement 7.2
 */
export interface ForEachConfig {
  /** Path to array in state */
  sourceArray: string;
  /** Variable name for current item (default: 'item') */
  itemVar: string;
  /** Variable name for current index (default: 'index') */
  indexVar: string;
}

/**
 * while loop configuration.
 */
export interface WhileConfig {
  /** Condition expression */
  condition: string;
}

/**
 * times loop configuration.
 */
export interface TimesConfig {
  /** Number of iterations or expression */
  count: number | string;
}

/**
 * Parallel execution configuration.
 * @see Requirement 7.3
 */
export interface ParallelConfig {
  enabled: boolean;
  /** Number of items to process in parallel */
  batchSize?: number;
  /** Delay between batches in milliseconds */
  delayBetween?: number;
}

/**
 * Result aggregation configuration.
 * @see Requirement 7.4
 */
export interface ResultsConfig {
  /** Collect results into array */
  collect: boolean;
  /** Key for aggregated results in state */
  aggregationKey?: string;
}

/**
 * Loop node configuration.
 * Iterates over arrays or repeats operations.
 * @see Requirement 7
 */
export interface LoopNodeConfig extends StandardProperties {
  type: 'loop';
  loopType: LoopType;
  forEach?: ForEachConfig;
  while?: WhileConfig;
  times?: TimesConfig;
  parallel: ParallelConfig;
  results: ResultsConfig;
}

// ============================================
// 7. MERGE NODE (Requirement 8)
// ============================================

/**
 * Merge mode.
 * @see Requirement 8.1
 */
export type MergeMode = 'wait_all' | 'wait_any' | 'wait_n';

/**
 * Combine strategy for merging branch outputs.
 * @see Requirement 8.2
 */
export type CombineStrategy = 'array' | 'object' | 'first' | 'last';

/**
 * Timeout behavior.
 * @see Requirement 8.3
 */
export type TimeoutBehavior = 'continue' | 'error';

/**
 * Merge timeout configuration.
 * @see Requirement 8.3
 */
export interface MergeTimeout {
  enabled: boolean;
  /** Timeout in milliseconds */
  ms: number;
  behavior: TimeoutBehavior;
}

/**
 * Merge node configuration.
 * Combines multiple branches back into single flow.
 * @see Requirement 8
 */
export interface MergeNodeConfig extends StandardProperties {
  type: 'merge';
  mode: MergeMode;
  /** Number of branches to wait for (for wait_n mode) */
  waitCount?: number;
  combineStrategy: CombineStrategy;
  /** Keys for object strategy */
  branchKeys?: string[];
  timeout: MergeTimeout;
}

// ============================================
// 8. WAIT NODE (Requirement 9)
// ============================================

/**
 * Wait type.
 * @see Requirement 9.1
 */
export type WaitType = 'fixed' | 'until' | 'webhook' | 'condition';

/**
 * Time unit for fixed duration.
 * @see Requirement 9.2
 */
export type TimeUnit = 'ms' | 's' | 'm' | 'h';

/**
 * Fixed duration configuration.
 * @see Requirement 9.2
 */
export interface FixedDuration {
  duration: number;
  unit: TimeUnit;
}

/**
 * Until timestamp configuration.
 */
export interface UntilConfig {
  /** ISO timestamp string or expression */
  timestamp: string;
}

/**
 * Webhook wait configuration.
 */
export interface WebhookWaitConfig {
  path: string;
  timeout: number;
}

/**
 * Condition polling configuration.
 * @see Requirement 9.3
 */
export interface ConditionPolling {
  /** Condition expression */
  expression: string;
  /** Poll interval in milliseconds */
  pollInterval: number;
  /** Maximum wait time in milliseconds */
  maxWait: number;
}

/**
 * Wait node configuration.
 * Pauses workflow execution for duration or condition.
 * @see Requirement 9
 */
export interface WaitNodeConfig extends StandardProperties {
  type: 'wait';
  waitType: WaitType;
  fixed?: FixedDuration;
  until?: UntilConfig;
  webhook?: WebhookWaitConfig;
  condition?: ConditionPolling;
}

// ============================================
// 9. CODE NODE (Requirement 10)
// ============================================

/**
 * Code language.
 * @see Requirement 10.1
 */
export type CodeLanguage = 'javascript' | 'typescript';

/**
 * Sandbox security configuration.
 * @see Requirement 10.2
 */
export interface SandboxConfig {
  /** Allow network access */
  networkAccess: boolean;
  /** Allow file system access */
  fileSystemAccess: boolean;
  /** Memory limit in MB */
  memoryLimit: number;
  /** Execution time limit in milliseconds */
  timeLimit: number;
}

/**
 * Code node configuration.
 * Executes custom JavaScript/TypeScript code in sandbox.
 * @see Requirement 10
 */
export interface CodeNodeConfig extends StandardProperties {
  type: 'code';
  language: CodeLanguage;
  /** Code to execute */
  code: string;
  sandbox: SandboxConfig;
  /** TypeScript type definition for input */
  inputType?: string;
  /** TypeScript type definition for output */
  outputType?: string;
}

// ============================================
// 10. DATABASE NODE (Requirement 11)
// ============================================

/**
 * Database type.
 * @see Requirement 11.1
 */
export type DatabaseType = 'postgresql' | 'mysql' | 'sqlite' | 'mongodb' | 'redis';

/**
 * Database connection configuration.
 * @see Requirement 11.2
 */
export interface DatabaseConnection {
  /** Connection string (marked as secret) */
  connectionString: string;
  /** Reference to Set node credentials */
  credentialRef?: string;
  /** Connection pool size */
  poolSize?: number;
}

/**
 * SQL operation type.
 * @see Requirement 11.3
 */
export type SqlOperation = 'query' | 'insert' | 'update' | 'delete' | 'upsert';

/**
 * SQL operations configuration.
 * @see Requirement 11.3
 */
export interface SqlConfig {
  operation: SqlOperation;
  /** Parameterized SQL query */
  query: string;
  /** Query parameters */
  params?: Record<string, unknown>;
}

/**
 * MongoDB operation type.
 * @see Requirement 11.4
 */
export type MongoOperation = 'find' | 'findOne' | 'insert' | 'update' | 'delete';

/**
 * MongoDB operations configuration.
 * @see Requirement 11.4
 */
export interface MongoConfig {
  collection: string;
  operation: MongoOperation;
  filter?: Record<string, unknown>;
  document?: Record<string, unknown>;
}

/**
 * Redis operation type.
 */
export type RedisOperation = 'get' | 'set' | 'del' | 'hget' | 'hset' | 'lpush' | 'rpop';

/**
 * Redis operations configuration.
 */
export interface RedisConfig {
  operation: RedisOperation;
  key: string;
  value?: unknown;
  /** TTL in seconds */
  ttl?: number;
}

/**
 * Database node configuration.
 * Performs database operations.
 * @see Requirement 11
 */
export interface DatabaseNodeConfig extends StandardProperties {
  type: 'database';
  dbType: DatabaseType;
  connection: DatabaseConnection;
  sql?: SqlConfig;
  mongodb?: MongoConfig;
  redis?: RedisConfig;
}

// ============================================
// 11. EMAIL NODE (Requirement 14)
// ============================================

/**
 * Email mode - monitor incoming or send outgoing.
 * @see Requirement 14.1, 14.2
 */
export type EmailMode = 'monitor' | 'send';

/**
 * IMAP connection configuration for monitoring emails.
 * @see Requirement 14.1
 */
export interface ImapConfig {
  /** IMAP server host */
  host: string;
  /** IMAP server port (default: 993 for SSL) */
  port: number;
  /** Use SSL/TLS */
  secure: boolean;
  /** Username (email address) */
  username: string;
  /** Password (marked as secret) */
  password: string;
  /** Folder to monitor (default: INBOX) */
  folder: string;
  /** Mark emails as read after processing */
  markAsRead: boolean;
}

/**
 * Email filter configuration for monitoring.
 * @see Requirement 14.1
 */
export interface EmailFilter {
  /** Filter by sender email/name */
  from?: string;
  /** Filter by subject (supports wildcards) */
  subject?: string;
  /** Filter by date range - start */
  dateFrom?: string;
  /** Filter by date range - end */
  dateTo?: string;
  /** Only unread emails */
  unreadOnly: boolean;
}

/**
 * SMTP connection configuration for sending emails.
 * @see Requirement 14.2
 */
export interface SmtpConfig {
  /** SMTP server host */
  host: string;
  /** SMTP server port (default: 587 for TLS, 465 for SSL) */
  port: number;
  /** Use SSL/TLS */
  secure: boolean;
  /** Username (email address) */
  username: string;
  /** Password (marked as secret) */
  password: string;
  /** From email address */
  fromEmail: string;
  /** From display name */
  fromName?: string;
}

/**
 * Email recipient configuration.
 * @see Requirement 14.2
 */
export interface EmailRecipients {
  /** To recipients (comma-separated or array) */
  to: string;
  /** CC recipients */
  cc?: string;
  /** BCC recipients */
  bcc?: string;
}

/**
 * Email body type.
 */
export type EmailBodyType = 'text' | 'html';

/**
 * Email content configuration for sending.
 * @see Requirement 14.2
 */
export interface EmailContent {
  /** Email subject (supports {{variable}} interpolation) */
  subject: string;
  /** Body type */
  bodyType: EmailBodyType;
  /** Email body (supports {{variable}} interpolation) */
  body: string;
}

/**
 * Email attachment configuration.
 * @see Requirement 14.3
 */
export interface EmailAttachment {
  /** Attachment filename */
  filename: string;
  /** State key containing file data (base64 or path) */
  stateKey: string;
  /** MIME type */
  mimeType?: string;
}

/**
 * Email node configuration.
 * Monitors incoming emails or sends outgoing emails.
 * @see Requirement 14
 */
export interface EmailNodeConfig extends StandardProperties {
  type: 'email';
  /** Email mode: monitor or send */
  mode: EmailMode;
  /** IMAP configuration for monitoring */
  imap?: ImapConfig;
  /** Email filters for monitoring */
  filters?: EmailFilter;
  /** SMTP configuration for sending */
  smtp?: SmtpConfig;
  /** Recipients for sending */
  recipients?: EmailRecipients;
  /** Email content for sending */
  content?: EmailContent;
  /** Attachments to send (from state) */
  attachments?: EmailAttachment[];
}

// ============================================
// 12. NOTIFICATION NODE (Requirement 17)
// ============================================

/**
 * Notification channel type.
 * @see Requirement 17.1
 */
export type NotificationChannel = 'slack' | 'discord' | 'teams' | 'webhook';

/**
 * Message format type.
 * @see Requirement 17.2
 */
export type MessageFormat = 'plain' | 'markdown' | 'blocks';

/**
 * Notification message configuration.
 * @see Requirement 17.2
 */
export interface NotificationMessage {
  /** Message text (supports {{variable}} interpolation) */
  text: string;
  /** Message format */
  format: MessageFormat;
  /** Block Kit (Slack) / Embeds (Discord) / Adaptive Cards (Teams) */
  blocks?: unknown[];
}

/**
 * Notification node configuration.
 * Sends notifications to Slack, Discord, Teams, or custom webhooks.
 * @see Requirement 17
 */
export interface NotificationNodeConfig extends StandardProperties {
  type: 'notification';
  /** Notification channel */
  channel: NotificationChannel;
  /** Webhook URL (marked as secret) */
  webhookUrl: string;
  /** Message configuration */
  message: NotificationMessage;
  /** Custom username for the notification */
  username?: string;
  /** Custom icon URL for the notification */
  iconUrl?: string;
  /** Target channel (for Slack) */
  targetChannel?: string;
}

// ============================================
// 13. RSS/FEED NODE (Requirement 15)
// ============================================

/**
 * RSS/Feed node configuration.
 * Monitors RSS/Atom feeds for new entries.
 * @see Requirement 15
 */

/**
 * Feed filter configuration for RSS monitoring.
 * @see Requirement 15.1
 */
export interface FeedFilter {
  /** Filter by keywords in title or description */
  keywords?: string[];
  /** Filter by author */
  author?: string;
  /** Filter by date - only entries after this date */
  dateFrom?: string;
  /** Filter by date - only entries before this date */
  dateTo?: string;
  /** Filter by category/tag */
  categories?: string[];
}

/**
 * Seen item tracking configuration.
 * @see Requirement 15.1
 */
export interface SeenItemTracking {
  /** Enable tracking of seen items to avoid duplicates */
  enabled: boolean;
  /** State key to store seen item IDs */
  stateKey: string;
  /** Maximum number of seen items to track */
  maxItems: number;
}

/**
 * RSS/Feed node configuration.
 * Monitors RSS/Atom feeds for new entries.
 * @see Requirement 15
 */
export interface RssNodeConfig extends StandardProperties {
  type: 'rss';
  /** Feed URL to monitor */
  feedUrl: string;
  /** Poll interval in milliseconds */
  pollInterval: number;
  /** Feed filters */
  filters?: FeedFilter;
  /** Seen item tracking configuration */
  seenTracking?: SeenItemTracking;
  /** Maximum number of entries to return per poll */
  maxEntries?: number;
  /** Include full content or just summary */
  includeContent: boolean;
  /** Parse media/enclosures */
  parseMedia: boolean;
}

// ============================================
// 14. FILE NODE (Requirement 16)
// ============================================

/**
 * File operation type.
 * @see Requirement 16.1
 */
export type FileOperation = 'read' | 'write' | 'delete' | 'list';

/**
 * File format for parsing.
 * @see Requirement 16.2
 */
export type FileFormat = 'json' | 'csv' | 'xml' | 'text' | 'binary';

/**
 * Cloud storage provider.
 * @see Requirement 16.3
 */
export type CloudProvider = 's3' | 'gcs' | 'azure';

/**
 * Local file configuration.
 * @see Requirement 16.1
 */
export interface LocalFileConfig {
  /** File path (supports {{variable}} interpolation) */
  path: string;
  /** File encoding (default: utf-8) */
  encoding?: string;
}

/**
 * CSV parsing options.
 * @see Requirement 16.2
 */
export interface CsvOptions {
  /** Field delimiter (default: ,) */
  delimiter: string;
  /** First row contains headers */
  hasHeader: boolean;
  /** Quote character (default: ") */
  quoteChar?: string;
  /** Escape character */
  escapeChar?: string;
}

/**
 * File parsing configuration.
 * @see Requirement 16.2
 */
export interface FileParseConfig {
  /** File format to parse */
  format: FileFormat;
  /** CSV-specific options */
  csvOptions?: CsvOptions;
  /** XML root element to extract */
  xmlRootElement?: string;
}

/**
 * Cloud storage configuration.
 * @see Requirement 16.3
 */
export interface CloudStorageConfig {
  /** Cloud provider */
  provider: CloudProvider;
  /** Bucket/container name */
  bucket: string;
  /** Object key/path */
  key: string;
  /** Credentials reference (state key or Set node reference) */
  credentials: string;
  /** AWS region (for S3) */
  region?: string;
  /** Generate presigned URL */
  presignedUrl?: boolean;
  /** Presigned URL expiration in seconds */
  presignedExpiry?: number;
}

/**
 * File write configuration.
 * @see Requirement 16.1
 */
export interface FileWriteConfig {
  /** Content to write (state key or expression) */
  content: string;
  /** Create parent directories if they don't exist */
  createDirs: boolean;
  /** Append to file instead of overwrite */
  append?: boolean;
}

/**
 * File list configuration.
 * @see Requirement 16.1
 */
export interface FileListConfig {
  /** Directory path to list */
  path: string;
  /** Include subdirectories recursively */
  recursive?: boolean;
  /** File pattern filter (glob) */
  pattern?: string;
}

/**
 * File node configuration.
 * Performs file operations on local or cloud storage.
 * @see Requirement 16
 */
export interface FileNodeConfig extends StandardProperties {
  type: 'file';
  /** File operation type */
  operation: FileOperation;
  /** Local file configuration */
  local?: LocalFileConfig;
  /** Cloud storage configuration */
  cloud?: CloudStorageConfig;
  /** File parsing options (for read operation) */
  parse?: FileParseConfig;
  /** File write options (for write operation) */
  write?: FileWriteConfig;
  /** File list options (for list operation) */
  list?: FileListConfig;
}

// ============================================
// Union Type & Type Guards
// ============================================

/**
 * Union type of all action node configurations.
 */
export type ActionNodeConfig =
  | TriggerNodeConfig
  | HttpNodeConfig
  | SetNodeConfig
  | TransformNodeConfig
  | SwitchNodeConfig
  | LoopNodeConfig
  | MergeNodeConfig
  | WaitNodeConfig
  | CodeNodeConfig
  | DatabaseNodeConfig
  | EmailNodeConfig
  | NotificationNodeConfig
  | RssNodeConfig
  | FileNodeConfig;

/**
 * Action node type identifier.
 */
export type ActionNodeType = ActionNodeConfig['type'];

/**
 * All action node types.
 */
export const ACTION_NODE_TYPES: ActionNodeType[] = [
  'trigger',
  'http',
  'set',
  'transform',
  'switch',
  'loop',
  'merge',
  'wait',
  'code',
  'database',
  'email',
  'notification',
  'rss',
  'file',
];

// Type guards for action nodes
export function isTriggerNode(node: ActionNodeConfig): node is TriggerNodeConfig {
  return node.type === 'trigger';
}

export function isHttpNode(node: ActionNodeConfig): node is HttpNodeConfig {
  return node.type === 'http';
}

export function isSetNode(node: ActionNodeConfig): node is SetNodeConfig {
  return node.type === 'set';
}

export function isTransformNode(node: ActionNodeConfig): node is TransformNodeConfig {
  return node.type === 'transform';
}

export function isSwitchNode(node: ActionNodeConfig): node is SwitchNodeConfig {
  return node.type === 'switch';
}

export function isLoopNode(node: ActionNodeConfig): node is LoopNodeConfig {
  return node.type === 'loop';
}

export function isMergeNode(node: ActionNodeConfig): node is MergeNodeConfig {
  return node.type === 'merge';
}

export function isWaitNode(node: ActionNodeConfig): node is WaitNodeConfig {
  return node.type === 'wait';
}

export function isCodeNode(node: ActionNodeConfig): node is CodeNodeConfig {
  return node.type === 'code';
}

export function isDatabaseNode(node: ActionNodeConfig): node is DatabaseNodeConfig {
  return node.type === 'database';
}

export function isEmailNode(node: ActionNodeConfig): node is EmailNodeConfig {
  return node.type === 'email';
}

export function isNotificationNode(node: ActionNodeConfig): node is NotificationNodeConfig {
  return node.type === 'notification';
}

export function isRssNode(node: ActionNodeConfig): node is RssNodeConfig {
  return node.type === 'rss';
}

export function isFileNode(node: ActionNodeConfig): node is FileNodeConfig {
  return node.type === 'file';
}
