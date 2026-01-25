# Action Nodes - Requirements

## Overview

Action Nodes are non-LLM programmatic nodes that enable deterministic operations in ADK Studio workflows. Inspired by n8n's approach, these nodes complement LLM agents by handling data transformation, API integrations, control flow, and automation logic.

## Glossary

| Term | Definition |
|------|------------|
| Action Node | A non-LLM node that performs programmatic operations |
| Standard Properties | Base properties shared by all action nodes (error handling, tracing, callbacks) |
| Functional Properties | Properties specific to each action node type |
| State | The shared data context passed between nodes in a workflow |
| Port | A connection point on a node for edges (input/output) |
| Branch | A conditional path in a Switch node |

---

## User Stories

### US-1: Workflow Automation Developer
As a workflow automation developer, I want to combine LLM agents with programmatic action nodes so that I can build end-to-end automation workflows that include both AI reasoning and deterministic operations.

### US-2: API Integration Builder
As an API integration builder, I want HTTP nodes with authentication support so that I can connect workflows to external services like Gmail, Slack, and Google Sheets.

### US-3: Data Pipeline Engineer
As a data pipeline engineer, I want transform and loop nodes so that I can process arrays of data and reshape information between workflow steps.

### US-4: No-Code Automator
As a no-code automator, I want visual configuration of action nodes so that I can build complex workflows without writing code.

---

## Requirements

### 1. Standard Node Properties

#### 1.1 Base Identity
THE system SHALL provide every action node with `id`, `name`, and optional `description` properties.

**Acceptance Criteria:**
- 1.1.1 Each node has a unique auto-generated ID
- 1.1.2 Node name is editable and displayed on the canvas
- 1.1.3 Description appears in tooltip on hover

#### 1.2 Error Handling
THE system SHALL provide standard error handling options for all action nodes.

**Acceptance Criteria:**
- 1.2.1 Error mode options: `stop`, `continue`, `retry`, `fallback`
- 1.2.2 Retry mode supports `retryCount` (1-10) and `retryDelay` (ms)
- 1.2.3 Fallback mode supports `fallbackValue` configuration
- 1.2.4 Error details are captured in execution state

#### 1.3 Tracing & Observability
THE system SHALL provide tracing and logging options for all action nodes.

**Acceptance Criteria:**
- 1.3.1 `enableTracing` toggle for detailed execution traces
- 1.3.2 `logLevel` options: `none`, `error`, `info`, `debug`
- 1.3.3 Traces appear in the execution timeline

#### 1.4 Callbacks
THE system SHALL support lifecycle callbacks for all action nodes.

**Acceptance Criteria:**
- 1.4.1 `onStart` callback fires before node execution
- 1.4.2 `onComplete` callback fires after successful execution
- 1.4.3 `onError` callback fires on execution failure

#### 1.5 Execution Control
THE system SHALL provide execution control options for all action nodes.

**Acceptance Criteria:**
- 1.5.1 `timeout` setting in milliseconds (default: 30000)
- 1.5.2 `condition` expression to skip node if false
- 1.5.3 Skipped nodes show "skipped" status in timeline

#### 1.6 Input/Output Mapping
THE system SHALL support input/output mapping for all action nodes.

**Acceptance Criteria:**
- 1.6.1 `inputMapping` maps state fields to node inputs
- 1.6.2 `outputKey` defines where result is stored in state
- 1.6.3 Supports dot notation for nested paths

---

### 2. Trigger Node

#### 2.1 Trigger Types
THE system SHALL support multiple trigger types for workflow entry points.

**Acceptance Criteria:**
- 2.1.1 `manual` trigger for user-initiated runs
- 2.1.2 `webhook` trigger with configurable path and method
- 2.1.3 `schedule` trigger with cron expression support
- 2.1.4 `event` trigger for external event sources

#### 2.2 Webhook Configuration
WHEN trigger type is `webhook`, THE system SHALL provide webhook configuration options.

**Acceptance Criteria:**
- 2.2.1 Configurable webhook path (e.g., `/api/webhook/my-flow`)
- 2.2.2 HTTP method selection (GET, POST)
- 2.2.3 Authentication options: none, bearer, api_key
- 2.2.4 Request body/params available in output

#### 2.3 Schedule Configuration
WHEN trigger type is `schedule`, THE system SHALL provide schedule configuration options.

**Acceptance Criteria:**
- 2.3.1 Cron expression input with validation
- 2.3.2 Timezone selection
- 2.3.3 Human-readable schedule preview

---

### 3. HTTP Node

#### 3.1 HTTP Methods
THE system SHALL support standard HTTP methods.

**Acceptance Criteria:**
- 3.1.1 Methods: GET, POST, PUT, PATCH, DELETE
- 3.1.2 URL field with variable interpolation `{{variable}}`
- 3.1.3 URL validation and preview

#### 3.2 Authentication
THE system SHALL support multiple authentication methods.

**Acceptance Criteria:**
- 3.2.1 No auth option
- 3.2.2 Bearer token authentication
- 3.2.3 Basic auth (username/password)
- 3.2.4 API key (configurable header name)
- 3.2.5 OAuth2 (future consideration)

#### 3.3 Request Configuration
THE system SHALL provide request body and header configuration.

**Acceptance Criteria:**
- 3.3.1 Headers as key-value pairs
- 3.3.2 Body types: none, JSON, form-data, raw
- 3.3.3 JSON body editor with syntax highlighting
- 3.3.4 Variable interpolation in body

#### 3.4 Response Handling
THE system SHALL provide response handling options.

**Acceptance Criteria:**
- 3.4.1 Response type: json, text, binary
- 3.4.2 Status code validation (e.g., "200-299")
- 3.4.3 JSONPath extraction from response
- 3.4.4 Full response available (status, headers, body)

#### 3.5 Rate Limiting
THE system SHALL support rate limiting configuration.

**Acceptance Criteria:**
- 3.5.1 Requests per time window setting
- 3.5.2 Automatic queuing when limit reached

---

### 4. Set Node

#### 4.1 Variable Definition
THE system SHALL allow defining variables in workflow state.

**Acceptance Criteria:**
- 4.1.1 Key-value pair configuration
- 4.1.2 Value types: string, number, boolean, JSON
- 4.1.3 Expression support in values `{{input.field}}`
- 4.1.4 Secret flag for sensitive values (masked in logs)

#### 4.2 Variable Operations
THE system SHALL support variable operations.

**Acceptance Criteria:**
- 4.2.1 `set` mode: create or overwrite
- 4.2.2 `merge` mode: deep merge with existing
- 4.2.3 `delete` mode: remove from state

#### 4.3 Environment Variables
THE system SHALL support environment variable loading.

**Acceptance Criteria:**
- 4.3.1 Load from .env file option
- 4.3.2 Filter by prefix option
- 4.3.3 Runtime environment access

---

### 5. Transform Node

#### 5.1 Transform Types
THE system SHALL support multiple transformation methods.

**Acceptance Criteria:**
- 5.1.1 JSONPath expressions
- 5.1.2 JMESPath expressions
- 5.1.3 Template strings (handlebars-style)
- 5.1.4 JavaScript expressions (sandboxed)

#### 5.2 Built-in Operations
THE system SHALL provide built-in transformation operations.

**Acceptance Criteria:**
- 5.2.1 `pick`: select specific fields
- 5.2.2 `omit`: exclude specific fields
- 5.2.3 `rename`: rename fields
- 5.2.4 `flatten`: flatten nested objects
- 5.2.5 `sort`: sort arrays
- 5.2.6 `unique`: deduplicate arrays

#### 5.3 Type Coercion
THE system SHALL support type coercion.

**Acceptance Criteria:**
- 5.3.1 Cast to string, number, boolean, array, object
- 5.3.2 Validation of cast result

---

### 6. Switch Node

#### 6.1 Condition Configuration
THE system SHALL support conditional branching.

**Acceptance Criteria:**
- 6.1.1 Multiple condition branches
- 6.1.2 Operators: ==, !=, >, <, >=, <=, contains, startsWith, endsWith, matches, in, empty, exists
- 6.1.3 Default/fallback branch
- 6.1.4 Visual branch indicators on canvas

#### 6.2 Evaluation Modes
THE system SHALL support different evaluation modes.

**Acceptance Criteria:**
- 6.2.1 `first_match`: stop at first matching condition
- 6.2.2 `all_match`: evaluate all conditions (multi-output)

#### 6.3 Expression Mode
THE system SHALL support expression-based routing.

**Acceptance Criteria:**
- 6.3.1 JavaScript expression returning branch name
- 6.3.2 Access to full state in expression

---

### 7. Loop Node

#### 7.1 Loop Types
THE system SHALL support multiple loop types.

**Acceptance Criteria:**
- 7.1.1 `forEach`: iterate over array
- 7.1.2 `while`: continue while condition true
- 7.1.3 `times`: repeat N times

#### 7.2 forEach Configuration
WHEN loop type is `forEach`, THE system SHALL provide array iteration options.

**Acceptance Criteria:**
- 7.2.1 Source array path configuration
- 7.2.2 Item variable name (default: `item`)
- 7.2.3 Index variable name (default: `index`)

#### 7.3 Parallel Execution
THE system SHALL support parallel loop execution.

**Acceptance Criteria:**
- 7.3.1 Parallel toggle
- 7.3.2 Batch size configuration
- 7.3.3 Delay between iterations option

#### 7.4 Result Aggregation
THE system SHALL support collecting loop results.

**Acceptance Criteria:**
- 7.4.1 Collect results toggle
- 7.4.2 Aggregation key configuration
- 7.4.3 Results available as array in state

---

### 8. Merge Node

#### 8.1 Merge Modes
THE system SHALL support multiple merge modes.

**Acceptance Criteria:**
- 8.1.1 `wait_all`: wait for all incoming branches
- 8.1.2 `wait_any`: continue on first branch completion
- 8.1.3 `wait_n`: wait for N branches

#### 8.2 Combine Strategies
THE system SHALL support different combine strategies.

**Acceptance Criteria:**
- 8.2.1 `array`: collect branch outputs into array
- 8.2.2 `object`: merge into object with branch keys
- 8.2.3 `first`: use first completed branch
- 8.2.4 `last`: use last completed branch

#### 8.3 Timeout Handling
THE system SHALL handle branch timeouts.

**Acceptance Criteria:**
- 8.3.1 Branch timeout configuration
- 8.3.2 Timeout behavior: continue or error

---

### 9. Wait Node

#### 9.1 Wait Types
THE system SHALL support multiple wait types.

**Acceptance Criteria:**
- 9.1.1 `fixed`: wait for duration
- 9.1.2 `until`: wait until timestamp
- 9.1.3 `webhook`: wait for external callback
- 9.1.4 `condition`: poll until condition true

#### 9.2 Fixed Duration
WHEN wait type is `fixed`, THE system SHALL provide duration configuration.

**Acceptance Criteria:**
- 9.2.1 Duration value input
- 9.2.2 Unit selection: ms, s, m, h

#### 9.3 Condition Polling
WHEN wait type is `condition`, THE system SHALL provide polling configuration.

**Acceptance Criteria:**
- 9.3.1 Condition expression
- 9.3.2 Poll interval configuration
- 9.3.3 Maximum wait timeout

---

### 10. Code Node

#### 10.1 Language Support
THE system SHALL support JavaScript code execution.

**Acceptance Criteria:**
- 10.1.1 JavaScript language support
- 10.1.2 TypeScript support (transpiled)
- 10.1.3 Syntax highlighting in editor

#### 10.2 Sandbox Security
THE system SHALL execute code in a secure sandbox.

**Acceptance Criteria:**
- 10.2.1 Network access toggle (default: off)
- 10.2.2 File system access toggle (default: off)
- 10.2.3 Memory limit configuration
- 10.2.4 Execution time limit

#### 10.3 Code Editor
THE system SHALL provide a code editor interface.

**Acceptance Criteria:**
- 10.3.1 Monaco editor integration
- 10.3.2 Input/output type hints
- 10.3.3 Error highlighting
- 10.3.4 Auto-completion for state variables

---

### 11. Database Node

#### 11.1 Database Types
THE system SHALL support multiple database types.

**Acceptance Criteria:**
- 11.1.1 PostgreSQL support
- 11.1.2 MySQL support
- 11.1.3 SQLite support
- 11.1.4 MongoDB support
- 11.1.5 Redis support

#### 11.2 Connection Configuration
THE system SHALL provide secure connection configuration.

**Acceptance Criteria:**
- 11.2.1 Connection string input (marked as secret)
- 11.2.2 Reference to Set node credentials
- 11.2.3 Connection pooling options

#### 11.3 SQL Operations
WHEN database type is SQL-based, THE system SHALL support SQL operations.

**Acceptance Criteria:**
- 11.3.1 Query operation with parameterized SQL
- 11.3.2 Insert operation
- 11.3.3 Update operation
- 11.3.4 Delete operation
- 11.3.5 Upsert operation

#### 11.4 NoSQL Operations
WHEN database type is MongoDB, THE system SHALL support document operations.

**Acceptance Criteria:**
- 11.4.1 Collection selection
- 11.4.2 Filter configuration
- 11.4.3 Document insert/update/delete

---

### 12. Visual Design

#### 12.1 Node Appearance
THE system SHALL visually distinguish action nodes from LLM agents.

**Acceptance Criteria:**
- 12.1.1 Distinct color scheme for action nodes (e.g., blue/purple vs green for LLM)
- 12.1.2 Icon for each action node type
- 12.1.3 Consistent sizing with LLM agent nodes

#### 12.2 Properties Panel
THE system SHALL provide a properties panel for action node configuration.

**Acceptance Criteria:**
- 12.2.1 Standard properties section (collapsible)
- 12.2.2 Functional properties section
- 12.2.3 JSON/code editors where appropriate
- 12.2.4 Validation feedback

#### 12.3 Canvas Integration
THE system SHALL integrate action nodes with existing canvas features.

**Acceptance Criteria:**
- 12.3.1 Drag-and-drop from palette
- 12.3.2 Edge connections to/from LLM agents
- 12.3.3 Execution highlighting during runs
- 12.3.4 State inspector shows action node outputs

---

### 13. Code Generation

#### 13.1 Rust Code Generation
THE system SHALL generate Rust code for action nodes.

**Acceptance Criteria:**
- 13.1.1 Each action node generates corresponding Rust code
- 13.1.2 HTTP node uses `reqwest` crate
- 13.1.3 Database node uses appropriate driver crates
- 13.1.4 Generated code compiles without errors

#### 13.2 Runtime Integration
THE system SHALL integrate action nodes with ADK runtime.

**Acceptance Criteria:**
- 13.2.1 Action nodes execute in workflow sequence
- 13.2.2 State passed between action nodes and LLM agents
- 13.2.3 Error handling respects node configuration


---

### 14. Email Node (NEW)

#### 14.1 Email Monitoring
THE system SHALL support email monitoring for workflow triggers.

**Acceptance Criteria:**
- 14.1.1 IMAP connection configuration
- 14.1.2 Folder selection (inbox, custom folders)
- 14.1.3 Filter by sender, subject, date range
- 14.1.4 Mark as read/unread after processing

#### 14.2 Email Sending
THE system SHALL support sending emails.

**Acceptance Criteria:**
- 14.2.1 SMTP configuration
- 14.2.2 To, CC, BCC recipients
- 14.2.3 Subject and body (HTML/plain text)
- 14.2.4 Attachment support from state

#### 14.3 Email Parsing
THE system SHALL parse email content and attachments.

**Acceptance Criteria:**
- 14.3.1 Extract headers (from, to, subject, date)
- 14.3.2 Extract body (plain text and HTML)
- 14.3.3 List attachments with metadata
- 14.3.4 Download attachments to state

---

### 15. RSS/Feed Node (NEW)

#### 15.1 Feed Monitoring
THE system SHALL support RSS/Atom feed monitoring.

**Acceptance Criteria:**
- 15.1.1 Feed URL configuration
- 15.1.2 Poll interval setting
- 15.1.3 Filter by keywords, date
- 15.1.4 Track seen items to avoid duplicates

#### 15.2 Feed Parsing
THE system SHALL parse feed entries.

**Acceptance Criteria:**
- 15.2.1 Extract title, link, description
- 15.2.2 Extract publish date and author
- 15.2.3 Extract media/enclosures
- 15.2.4 Support both RSS 2.0 and Atom formats

---

### 16. File Node (NEW)

#### 16.1 File Operations
THE system SHALL support file operations.

**Acceptance Criteria:**
- 16.1.1 Read file from path or URL
- 16.1.2 Write file to path
- 16.1.3 Delete file
- 16.1.4 List directory contents

#### 16.2 File Parsing
THE system SHALL support parsing common file formats.

**Acceptance Criteria:**
- 16.2.1 JSON file parsing
- 16.2.2 CSV file parsing with header detection
- 16.2.3 XML file parsing
- 16.2.4 Plain text reading

#### 16.3 Cloud Storage
THE system SHALL support cloud storage integration.

**Acceptance Criteria:**
- 16.3.1 S3-compatible storage (AWS S3, MinIO)
- 16.3.2 Google Cloud Storage
- 16.3.3 Azure Blob Storage
- 16.3.4 Presigned URL generation

---

### 17. Notification Node (NEW)

#### 17.1 Notification Channels
THE system SHALL support multiple notification channels.

**Acceptance Criteria:**
- 17.1.1 Slack webhook integration
- 17.1.2 Discord webhook integration
- 17.1.3 Microsoft Teams webhook integration
- 17.1.4 Generic webhook for custom services

#### 17.2 Message Formatting
THE system SHALL support rich message formatting.

**Acceptance Criteria:**
- 17.2.1 Plain text messages
- 17.2.2 Markdown formatting
- 17.2.3 Block Kit (Slack) / Embeds (Discord)
- 17.2.4 Variable interpolation in messages

---

### 18. Vector Search Node (NEW - Stretch)

#### 18.1 Vector Database Connection
THE system SHALL support vector database integration.

**Acceptance Criteria:**
- 18.1.1 Pinecone connection
- 18.1.2 Weaviate connection
- 18.1.3 Qdrant connection
- 18.1.4 ChromaDB connection

#### 18.2 Vector Operations
THE system SHALL support vector operations.

**Acceptance Criteria:**
- 18.2.1 Upsert vectors with metadata
- 18.2.2 Semantic search by text query
- 18.2.3 Search by vector embedding
- 18.2.4 Filter by metadata
- 18.2.5 Delete vectors by ID or filter

---

### 19. Document Parser Node (NEW - Stretch)

#### 19.1 Document Types
THE system SHALL support parsing various document types.

**Acceptance Criteria:**
- 19.1.1 PDF text extraction
- 19.1.2 Word document (.docx) parsing
- 19.1.3 Excel/CSV parsing with sheet selection
- 19.1.4 Image OCR (via external API)

#### 19.2 Extraction Options
THE system SHALL provide extraction configuration.

**Acceptance Criteria:**
- 19.2.1 Full text extraction
- 19.2.2 Page-by-page extraction (PDF)
- 19.2.3 Table extraction
- 19.2.4 Metadata extraction (author, date, etc.)

---

## Use Case Validation

The following n8n workflows can be achieved with our action nodes:

### UC-1: AI-Powered Lead Generation ✅
- Trigger (webhook) → HTTP (form data) → LLM Agent (analyze) → Switch (score routing) → HTTP (CRM) → Notification (Slack)

### UC-2: Intelligent Email Marketing ✅
- Trigger (schedule) → Database (fetch customers) → Loop (forEach) → LLM Agent (personalize) → Email (send) → Wait (monitor) → LLM Agent (follow-up)

### UC-3: Social Media AI Content Creator ✅
- Trigger (RSS/webhook) → Transform (extract) → LLM Agent (adapt per platform) → Loop (platforms) → HTTP (post APIs)

### UC-4: AI Customer Support ✅
- Trigger (webhook) → LLM Agent (classify) → Vector Search (knowledge base) → LLM Agent (generate response) → Switch (confidence routing) → Email/Notification

### UC-5: Intelligent Data Analysis ✅
- Trigger (schedule) → Database (extract data) → Transform (clean) → LLM Agent (analyze trends) → LLM Agent (generate report) → File (save PDF) → Email (distribute)

### UC-6: AI Invoice Processing ✅
- Email (monitor) → Document Parser (OCR) → LLM Agent (validate/categorize) → Switch (approval routing) → Database (accounting) → Email (confirmation)

### UC-7: AI Job Application Bot ✅
- RSS (job postings) → LLM Agent (analyze requirements) → LLM Agent (generate proposal) → HTTP (submit) → Database (track)

### UC-8: AI Newsletter Digest ✅
- Email (fetch newsletters) → Loop (forEach) → LLM Agent (summarize) → Transform (cluster topics) → LLM Agent (generate digest) → Email (send)

### UC-9: E-commerce Order Intelligence ✅
- Trigger (webhook) → LLM Agent (fraud detection) → Database (inventory) → HTTP (shipping API) → LLM Agent (personalize message) → Email/Notification

### UC-10: AI Incident Response ✅
- Trigger (webhook) → LLM Agent (analyze logs) → Vector Search (past incidents) → LLM Agent (generate summary) → Switch (severity) → Notification (PagerDuty/Slack)

### UC-11: AI Code Review ✅
- Trigger (webhook) → HTTP (fetch diff) → LLM Agent (analyze code) → LLM Agent (suggest tests) → HTTP (post comments) → Notification (Slack)

### UC-12: AI Customer Onboarding ✅
- Trigger (webhook) → LLM Agent (predict needs) → Database (create profile) → Loop (email sequence) → LLM Agent (personalize) → Email (send) → Wait (monitor) → Switch (engagement routing)

