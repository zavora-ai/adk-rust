# Implementation Plan: Action Nodes

## Overview

This implementation plan breaks down the Action Nodes feature into discrete, incremental tasks. The feature adds 10 non-LLM programmatic nodes to ADK Studio for deterministic workflow operations.

**Estimated Timeline**: 4-6 weeks

**Dependencies**: ADK Studio v2.0 core features (theme, layout, canvas)

## Tasks

- [-] 1. Foundation: Type Definitions & Store
  - [ ] 1.1 Create standard properties type definitions
    - Create `src/types/standardProperties.ts`
    - Define StandardProperties interface with error handling, tracing, callbacks, execution, mapping
    - _Requirements: 1.1-1.6_
  
  - [ ] 1.2 Create action node type definitions
    - Create `src/types/actionNodes.ts`
    - Define all 10 action node config interfaces
    - Create ActionNodeConfig union type
    - _Requirements: 2-11_
  
  - [ ] 1.3 Extend project store with action nodes
    - Update `src/store/index.ts` to include actionNodes record
    - Add CRUD operations for action nodes
    - _Requirements: 12.3_
  
  - [ ] 1.4 Write property test for standard properties persistence
    - **Property 1: Standard Properties Persistence**
    - **Validates: Requirements 1.1-1.6**

- [ ] 2. Checkpoint - Type System Complete
  - Verify all types compile, store operations work

- [ ] 3. Visual Components: Base & Palette
  - [ ] 3.1 Create action node CSS styles
    - Create `src/styles/actionNodes.css`
    - Define colors, icons, and visual styles for all 10 types
    - Support light/dark themes
    - _Requirements: 12.1_

  - [ ] 3.2 Create ActionNodeBase component
    - Create `src/components/ActionNodes/ActionNodeBase.tsx`
    - Implement shared wrapper with header, status indicator, handles
    - Support multiple input/output ports
    - _Requirements: 12.1, 12.3_
  
  - [ ] 3.3 Create ActionPalette component
    - Create `src/components/ActionPalette/ActionPalette.tsx`
    - Display all 10 action node types as draggable items
    - Group by category (Entry, Data, Control, Integration)
    - _Requirements: 12.3_
  
  - [ ] 3.4 Integrate ActionPalette into sidebar
    - Add new tab/section for action nodes
    - Wire drag-and-drop to canvas
    - _Requirements: 12.3_
  
  - [ ] 3.5 Write property test for action node visual distinction
    - **Property 9: Action Node Visual Distinction**
    - **Validates: Requirements 12.1**

- [ ] 4. Checkpoint - Base Components Complete
  - Verify action nodes can be added to canvas

- [ ] 5. Properties Panel: Standard Properties
  - [ ] 5.1 Create StandardPropertiesPanel component
    - Create `src/components/ActionPanels/StandardPropertiesPanel.tsx`
    - Implement collapsible sections for each property group
    - _Requirements: 12.2_
  
  - [ ] 5.2 Implement error handling section
    - Mode selector (stop/continue/retry/fallback)
    - Conditional fields for retry count, delay, fallback value
    - _Requirements: 1.2_
  
  - [ ] 5.3 Implement tracing section
    - Enable toggle and log level selector
    - _Requirements: 1.3_
  
  - [ ] 5.4 Implement callbacks section
    - Text inputs for onStart, onComplete, onError
    - _Requirements: 1.4_
  
  - [ ] 5.5 Implement execution control section
    - Timeout input and condition expression
    - _Requirements: 1.5_
  
  - [ ] 5.6 Implement input/output mapping section
    - Key-value editor for input mapping
    - Output key input
    - _Requirements: 1.6_

- [ ] 6. Checkpoint - Standard Properties Complete
  - Verify all standard properties can be configured

- [ ] 7. Node Implementation: Trigger
  - [ ] 7.1 Create TriggerNode component
    - Create `src/components/ActionNodes/TriggerNode.tsx`
    - Display trigger type badge
    - _Requirements: 2.1_
  
  - [ ] 7.2 Create TriggerPanel component
    - Create `src/components/ActionPanels/TriggerPanel.tsx`
    - Trigger type selector
    - _Requirements: 2.1_
  
  - [ ] 7.3 Implement webhook configuration
    - Path, method, auth options
    - _Requirements: 2.2_
  
  - [ ] 7.4 Implement schedule configuration
    - Cron expression input with validation
    - Timezone selector
    - Human-readable preview
    - _Requirements: 2.3_

- [ ] 8. Node Implementation: HTTP
  - [ ] 8.1 Create HttpNode component
    - Create `src/components/ActionNodes/HttpNode.tsx`
    - Display method and URL preview
    - _Requirements: 3.1_
  
  - [ ] 8.2 Create HttpPanel component
    - Create `src/components/ActionPanels/HttpPanel.tsx`
    - Method selector and URL input
    - _Requirements: 3.1_
  
  - [ ] 8.3 Implement authentication section
    - Auth type selector with conditional fields
    - _Requirements: 3.2_
  
  - [ ] 8.4 Implement headers and body section
    - Key-value editor for headers
    - Body type selector with JSON editor
    - _Requirements: 3.3_
  
  - [ ] 8.5 Implement response handling section
    - Response type, status validation, JSONPath extraction
    - _Requirements: 3.4_
  
  - [ ] 8.6 Write property test for variable interpolation
    - **Property 3: HTTP Variable Interpolation**
    - **Validates: Requirements 3.1**

- [ ] 9. Node Implementation: Set
  - [ ] 9.1 Create SetNode component
    - Create `src/components/ActionNodes/SetNode.tsx`
    - Display variable count badge
    - _Requirements: 4.1_
  
  - [ ] 9.2 Create SetPanel component
    - Create `src/components/ActionPanels/SetPanel.tsx`
    - Mode selector (set/merge/delete)
    - Variable list editor
    - _Requirements: 4.1, 4.2_
  
  - [ ] 9.3 Implement environment variable loading
    - Load from .env toggle
    - Prefix filter
    - _Requirements: 4.3_

- [ ] 10. Node Implementation: Transform
  - [ ] 10.1 Create TransformNode component
    - Create `src/components/ActionNodes/TransformNode.tsx`
    - Display transform type badge
    - _Requirements: 5.1_
  
  - [ ] 10.2 Create TransformPanel component
    - Create `src/components/ActionPanels/TransformPanel.tsx`
    - Transform type selector
    - Expression editor with syntax highlighting
    - _Requirements: 5.1_
  
  - [ ] 10.3 Implement built-in operations UI
    - Operation list with add/remove
    - Operation-specific config
    - _Requirements: 5.2_
  
  - [ ] 10.4 Implement type coercion section
    - Target type selector
    - _Requirements: 5.3_

- [ ] 11. Node Implementation: Switch
  - [ ] 11.1 Create SwitchNode component
    - Create `src/components/ActionNodes/SwitchNode.tsx`
    - Display branch count and multiple output handles
    - _Requirements: 6.1_
  
  - [ ] 11.2 Create SwitchPanel component
    - Create `src/components/ActionPanels/SwitchPanel.tsx`
    - Evaluation mode selector
    - _Requirements: 6.2_
  
  - [ ] 11.3 Implement condition builder
    - Condition list with add/remove
    - Field, operator, value inputs
    - Output port assignment
    - _Requirements: 6.1_
  
  - [ ] 11.4 Implement expression mode
    - Toggle for expression mode
    - JavaScript expression editor
    - _Requirements: 6.3_
  
  - [ ] 11.5 Write property test for condition evaluation
    - **Property 4: Switch Condition Evaluation**
    - **Validates: Requirements 6.1, 6.2**

- [ ] 12. Node Implementation: Loop
  - [ ] 12.1 Create LoopNode component
    - Create `src/components/ActionNodes/LoopNode.tsx`
    - Display loop type and iteration indicator
    - _Requirements: 7.1_
  
  - [ ] 12.2 Create LoopPanel component
    - Create `src/components/ActionPanels/LoopPanel.tsx`
    - Loop type selector
    - _Requirements: 7.1_
  
  - [ ] 12.3 Implement forEach configuration
    - Source array path, item/index variable names
    - _Requirements: 7.2_
  
  - [ ] 12.4 Implement parallel execution section
    - Parallel toggle, batch size, delay
    - _Requirements: 7.3_
  
  - [ ] 12.5 Implement result aggregation section
    - Collect toggle, aggregation key
    - _Requirements: 7.4_
  
  - [ ] 12.6 Write property test for result aggregation
    - **Property 5: Loop Result Aggregation**
    - **Validates: Requirements 7.4**

- [ ] 13. Node Implementation: Merge
  - [ ] 13.1 Create MergeNode component
    - Create `src/components/ActionNodes/MergeNode.tsx`
    - Display mode and multiple input handles
    - _Requirements: 8.1_
  
  - [ ] 13.2 Create MergePanel component
    - Create `src/components/ActionPanels/MergePanel.tsx`
    - Mode selector (wait_all/wait_any/wait_n)
    - Combine strategy selector
    - _Requirements: 8.1, 8.2_
  
  - [ ] 13.3 Implement timeout handling section
    - Timeout toggle, duration, behavior
    - _Requirements: 8.3_
  
  - [ ] 13.4 Write property test for wait behavior
    - **Property 6: Merge Wait Behavior**
    - **Validates: Requirements 8.1**

- [ ] 14. Node Implementation: Wait
  - [ ] 14.1 Create WaitNode component
    - Create `src/components/ActionNodes/WaitNode.tsx`
    - Display wait type and duration preview
    - _Requirements: 9.1_
  
  - [ ] 14.2 Create WaitPanel component
    - Create `src/components/ActionPanels/WaitPanel.tsx`
    - Wait type selector
    - _Requirements: 9.1_
  
  - [ ] 14.3 Implement fixed duration configuration
    - Duration input with unit selector
    - _Requirements: 9.2_
  
  - [ ] 14.4 Implement condition polling configuration
    - Expression, poll interval, max wait
    - _Requirements: 9.3_

- [ ] 15. Node Implementation: Code
  - [ ] 15.1 Create CodeNode component
    - Create `src/components/ActionNodes/CodeNode.tsx`
    - Display language badge
    - _Requirements: 10.1_
  
  - [ ] 15.2 Create CodePanel component
    - Create `src/components/ActionPanels/CodePanel.tsx`
    - Language selector
    - Monaco editor integration
    - _Requirements: 10.1, 10.3_
  
  - [ ] 15.3 Implement sandbox configuration
    - Network/filesystem access toggles
    - Memory/time limits
    - _Requirements: 10.2_
  
  - [ ] 15.4 Write property test for sandbox isolation
    - **Property 7: Code Sandbox Isolation**
    - **Validates: Requirements 10.2**

- [ ] 16. Node Implementation: Database
  - [ ] 16.1 Create DatabaseNode component
    - Create `src/components/ActionNodes/DatabaseNode.tsx`
    - Display database type badge
    - _Requirements: 11.1_
  
  - [ ] 16.2 Create DatabasePanel component
    - Create `src/components/ActionPanels/DatabasePanel.tsx`
    - Database type selector
    - _Requirements: 11.1_
  
  - [ ] 16.3 Implement connection configuration
    - Connection string (secret input)
    - Credential reference selector
    - _Requirements: 11.2_
  
  - [ ] 16.4 Implement SQL operations UI
    - Operation selector
    - Query editor with syntax highlighting
    - Parameter editor
    - _Requirements: 11.3_
  
  - [ ] 16.5 Implement MongoDB operations UI
    - Collection input
    - Filter/document JSON editors
    - _Requirements: 11.4_
  
  - [ ] 16.6 Write property test for connection security
    - **Property 8: Database Connection Security**
    - **Validates: Requirements 11.2**

- [ ] 17. Checkpoint - All Nodes Complete
  - Verify all 10 node types can be configured

- [ ] 18. Canvas Integration
  - [ ] 18.1 Register action node types with ReactFlow
    - Update node type registry
    - Handle multi-port connections
    - _Requirements: 12.3_
  
  - [ ] 18.2 Implement edge connections between action nodes and agents
    - Allow connections in both directions
    - Validate connection compatibility
    - _Requirements: 12.3_
  
  - [ ] 18.3 Implement execution highlighting for action nodes
    - Status updates during workflow run
    - _Requirements: 12.3_
  
  - [ ] 18.4 Integrate action nodes with state inspector
    - Show action node outputs in inspector
    - _Requirements: 12.3_

- [ ] 19. Checkpoint - Canvas Integration Complete
  - Verify action nodes work on canvas with agents

- [ ] 20. Code Generation: Backend
  - [ ] 20.1 Create action node code generation module
    - Create `adk-studio/src/codegen/action_nodes.rs`
    - Define code generation traits
    - _Requirements: 13.1_
  
  - [ ] 20.2 Implement Trigger node code generation
    - Generate webhook handler or cron setup
    - _Requirements: 13.1_
  
  - [ ] 20.3 Implement HTTP node code generation
    - Generate reqwest-based HTTP calls
    - Handle auth, headers, body
    - _Requirements: 13.1, 13.2_
  
  - [ ] 20.4 Implement Set node code generation
    - Generate state manipulation code
    - _Requirements: 13.1_
  
  - [ ] 20.5 Implement Transform node code generation
    - Generate transformation logic
    - _Requirements: 13.1_
  
  - [ ] 20.6 Implement Switch node code generation
    - Generate conditional branching
    - _Requirements: 13.1_
  
  - [ ] 20.7 Implement Loop node code generation
    - Generate iteration with optional parallelism
    - _Requirements: 13.1_
  
  - [ ] 20.8 Implement Merge node code generation
    - Generate branch synchronization
    - _Requirements: 13.1_
  
  - [ ] 20.9 Implement Wait node code generation
    - Generate delay/polling logic
    - _Requirements: 13.1_
  
  - [ ] 20.10 Implement Code node code generation
    - Generate sandboxed execution wrapper
    - _Requirements: 13.1_
  
  - [ ] 20.11 Implement Database node code generation
    - Generate sqlx/mongodb client code
    - _Requirements: 13.1, 13.3_
  
  - [ ] 20.12 Implement error handling wrapper generation
    - Generate retry, fallback, continue logic
    - _Requirements: 13.2_
  
  - [ ] 20.13 Write property test for code generation validity
    - **Property 10: Code Generation Validity**
    - **Validates: Requirements 13.1**

- [ ] 21. Checkpoint - Code Generation Complete
  - Verify generated code compiles

- [ ] 22. Runtime Integration
  - [ ] 22.1 Extend workflow executor for action nodes
    - Update `adk-studio/src/runner.rs`
    - Handle action node execution
    - _Requirements: 13.2_
  
  - [ ] 22.2 Implement state passing between action nodes and agents
    - Ensure state flows correctly
    - _Requirements: 13.2_
  
  - [ ] 22.3 Implement SSE events for action nodes
    - Emit action_start, action_end, action_error events
    - Include state snapshots
    - _Requirements: 13.2_
  
  - [ ] 22.4 Write property test for error handling behavior
    - **Property 2: Error Handling Mode Behavior**
    - **Validates: Requirements 1.2**

- [ ] 23. Checkpoint - Runtime Integration Complete
  - Verify action nodes execute in workflows

- [ ] 24. Testing & Documentation
  - [ ] 24.1 Create example workflow: Email Sentiment Analysis
    - Implement the use case from design doc
    - Test end-to-end execution
  
  - [ ] 24.2 Update ADK Studio README
    - Document action node types
    - Add configuration examples
  
  - [ ] 24.3 Add tooltips to all action node UI elements
  
  - [ ] 24.4 Final UI polish pass
    - Consistent styling across all panels
    - Responsive layout

- [ ] 25. Final Checkpoint - All Tests Pass
  - Run full test suite
  - Verify all 10 property tests pass

---

## Property Tests Summary

| # | Property | Requirements |
|---|----------|--------------|
| 1 | Standard Properties Persistence | 1.1-1.6 |
| 2 | Error Handling Mode Behavior | 1.2 |
| 3 | HTTP Variable Interpolation | 3.1 |
| 4 | Switch Condition Evaluation | 6.1, 6.2 |
| 5 | Loop Result Aggregation | 7.4 |
| 6 | Merge Wait Behavior | 8.1 |
| 7 | Code Sandbox Isolation | 10.2 |
| 8 | Database Connection Security | 11.2 |
| 9 | Action Node Visual Distinction | 12.1 |
| 10 | Code Generation Validity | 13.1 |

---

## Notes

- All action nodes share standard properties (error handling, tracing, callbacks, execution control, mapping)
- Switch and Merge nodes have multiple ports requiring special canvas handling
- Code node requires sandboxed JavaScript execution (quickjs-rs or similar)
- Database node requires multiple driver dependencies (sqlx, mongodb)
- Code generation must produce valid, compilable Rust code


---

## Phase 2: Communication Nodes

- [ ] 26. Node Implementation: Email
  - [ ] 26.1 Create EmailNode component
    - Create `src/components/ActionNodes/EmailNode.tsx`
    - Display mode badge (monitor/send)
    - _Requirements: 14.1, 14.2_
  
  - [ ] 26.2 Create EmailPanel component
    - Create `src/components/ActionPanels/EmailPanel.tsx`
    - Mode selector (monitor/send)
    - _Requirements: 14.1, 14.2_
  
  - [ ] 26.3 Implement IMAP monitoring configuration
    - Host, port, credentials (secret)
    - Folder selection, filters
    - _Requirements: 14.1_
  
  - [ ] 26.4 Implement SMTP sending configuration
    - Host, port, credentials (secret)
    - Recipients, subject, body editor
    - _Requirements: 14.2_
  
  - [ ] 26.5 Implement attachment handling
    - List attachments from monitored emails
    - Attach files from state when sending
    - _Requirements: 14.3_
  
  - [ ] 26.6 Implement Email code generation
    - Generate lettre/mail crate code for Rust
    - _Requirements: 13.1_

- [ ] 27. Node Implementation: Notification
  - [ ] 27.1 Create NotificationNode component
    - Create `src/components/ActionNodes/NotificationNode.tsx`
    - Display channel badge (Slack/Discord/Teams)
    - _Requirements: 17.1_
  
  - [ ] 27.2 Create NotificationPanel component
    - Create `src/components/ActionPanels/NotificationPanel.tsx`
    - Channel selector
    - Webhook URL (secret)
    - _Requirements: 17.1_
  
  - [ ] 27.3 Implement message formatting
    - Plain text, markdown, blocks editor
    - Variable interpolation preview
    - _Requirements: 17.2_
  
  - [ ] 27.4 Implement Notification code generation
    - Generate webhook POST code
    - _Requirements: 13.1_

- [ ] 28. Checkpoint - Communication Nodes Complete
  - Verify Email and Notification nodes work

---

## Phase 3: Data Source Nodes

- [ ] 29. Node Implementation: RSS/Feed
  - [ ] 29.1 Create RssNode component
    - Create `src/components/ActionNodes/RssNode.tsx`
    - Display feed URL preview
    - _Requirements: 15.1_
  
  - [ ] 29.2 Create RssPanel component
    - Create `src/components/ActionPanels/RssPanel.tsx`
    - Feed URL input
    - Poll interval configuration
    - _Requirements: 15.1_
  
  - [ ] 29.3 Implement feed filtering
    - Keywords, date filters
    - Seen item tracking
    - _Requirements: 15.1, 15.2_
  
  - [ ] 29.4 Implement RSS code generation
    - Generate feed-rs crate code
    - _Requirements: 13.1_

- [ ] 30. Node Implementation: File
  - [ ] 30.1 Create FileNode component
    - Create `src/components/ActionNodes/FileNode.tsx`
    - Display operation badge (read/write/delete)
    - _Requirements: 16.1_
  
  - [ ] 30.2 Create FilePanel component
    - Create `src/components/ActionPanels/FilePanel.tsx`
    - Operation selector
    - Path/URL input
    - _Requirements: 16.1_
  
  - [ ] 30.3 Implement file parsing options
    - Format selector (JSON/CSV/XML/text)
    - CSV options (delimiter, header)
    - _Requirements: 16.2_
  
  - [ ] 30.4 Implement cloud storage configuration
    - Provider selector (S3/GCS/Azure)
    - Bucket, key, credentials
    - _Requirements: 16.3_
  
  - [ ] 30.5 Implement File code generation
    - Generate tokio::fs and cloud SDK code
    - _Requirements: 13.1_

- [ ] 31. Checkpoint - Data Source Nodes Complete
  - Verify RSS and File nodes work

---

## Phase 4: AI Enhancement Nodes (Stretch)

- [ ]* 32. Node Implementation: Vector Search
  - [ ]* 32.1 Create VectorSearchNode component
    - Create `src/components/ActionNodes/VectorSearchNode.tsx`
    - Display provider badge
    - _Requirements: 18.1_
  
  - [ ]* 32.2 Create VectorSearchPanel component
    - Create `src/components/ActionPanels/VectorSearchPanel.tsx`
    - Provider selector
    - Connection configuration
    - _Requirements: 18.1_
  
  - [ ]* 32.3 Implement vector operations
    - Upsert, search, delete operations
    - Filter configuration
    - _Requirements: 18.2_
  
  - [ ]* 32.4 Implement Vector Search code generation
    - Generate provider-specific SDK code
    - _Requirements: 13.1_

- [ ]* 33. Node Implementation: Document Parser
  - [ ]* 33.1 Create DocumentParserNode component
    - Create `src/components/ActionNodes/DocumentParserNode.tsx`
    - Display document type badge
    - _Requirements: 19.1_
  
  - [ ]* 33.2 Create DocumentParserPanel component
    - Create `src/components/ActionPanels/DocumentParserPanel.tsx`
    - Document type selector
    - Source configuration
    - _Requirements: 19.1_
  
  - [ ]* 33.3 Implement extraction options
    - Mode selector (full/pages/tables)
    - OCR configuration
    - _Requirements: 19.2_
  
  - [ ]* 33.4 Implement Document Parser code generation
    - Generate pdf-extract, docx-rs code
    - _Requirements: 13.1_

- [ ]* 34. Checkpoint - AI Enhancement Nodes Complete
  - Verify Vector Search and Document Parser nodes work

---

## Phase 5: Workflow Templates

- [ ] 35. Create n8n-Inspired Workflow Templates
  - [ ] 35.1 Create template data structure for action node workflows
    - Extend existing template system to support action nodes
    - Define template schema with agents + action nodes + edges
    - _Requirements: Use Case Validation_
  
  - [ ] 35.2 Template 1: AI-Powered Lead Generation
    - Trigger (webhook) → HTTP (form) → LLM Agent (analyze) → LLM Agent (score) → Switch (routing) → HTTP (CRM) → Notification (Slack)
    - Include sample prompts and configurations
  
  - [ ] 35.3 Template 2: Intelligent Email Marketing
    - Trigger (schedule) → Database (customers) → Loop (forEach) → LLM Agent (personalize) → Email (send) → Wait (monitor)
    - Include email templates and segmentation logic
  
  - [ ] 35.4 Template 3: Social Media AI Content Creator
    - Trigger (RSS/webhook) → Transform (extract) → Loop (platforms) → LLM Agent (adapt) → HTTP (post APIs)
    - Include platform-specific prompt templates
  
  - [ ] 35.5 Template 4: AI Customer Support
    - Trigger (webhook) → LLM Agent (classify) → Vector Search (KB) → LLM Agent (respond) → Switch (confidence) → Email/Notification
    - Include knowledge base setup instructions
  
  - [ ] 35.6 Template 5: Intelligent Data Analysis & Reporting
    - Trigger (schedule) → Database (extract) → Transform (clean) → LLM Agent (analyze) → LLM Agent (report) → File (save) → Email (distribute)
    - Include sample SQL queries and report templates
  
  - [ ] 35.7 Template 6: AI Invoice Processing
    - Email (monitor) → Document Parser (OCR) → LLM Agent (categorize) → Switch (approval) → Database (accounting) → Email (confirm)
    - Include invoice parsing prompts
  
  - [ ] 35.8 Template 7: AI Job Application Bot
    - RSS (job postings) → LLM Agent (analyze) → LLM Agent (match score) → Switch (filter) → LLM Agent (proposal) → HTTP (submit) → Database (track)
    - Include proposal generation prompts
  
  - [ ] 35.9 Template 8: AI Newsletter Digest Creator
    - Email (fetch) → Loop (newsletters) → LLM Agent (summarize) → Transform (cluster) → LLM Agent (digest) → Email (send)
    - Include summarization prompts
  
  - [ ] 35.10 Template 9: E-commerce AI Order Intelligence
    - Trigger (webhook) → LLM Agent (fraud check) → Database (inventory) → HTTP (shipping) → LLM Agent (personalize) → Email/Notification
    - Include fraud detection prompts
  
  - [ ] 35.11 Template 10: AI-Powered Incident Response
    - Trigger (webhook) → LLM Agent (analyze logs) → Vector Search (past incidents) → LLM Agent (summary) → Switch (severity) → Notification (PagerDuty) → LLM Agent (runbook)
    - Include incident classification prompts
  
  - [ ] 35.12 Template 11: AI Code Review & QA
    - Trigger (webhook) → HTTP (fetch diff) → LLM Agent (review) → LLM Agent (security scan) → LLM Agent (suggest tests) → HTTP (post comments) → Notification
    - Include code review prompts
  
  - [ ] 35.13 Template 12: AI Customer Onboarding Journey
    - Trigger (webhook) → LLM Agent (predict needs) → Database (profile) → Loop (email sequence) → LLM Agent (personalize) → Email (send) → Wait (monitor) → Switch (engagement) → LLM Agent (intervention)
    - Include onboarding email templates
  
  - [ ] 35.14 Integrate templates into TemplateGallery
    - Add "Automation" category for action node templates
    - Show action node icons in template previews
    - Filter templates by required node types
  
  - [ ] 35.15 Add template documentation
    - README for each template explaining use case
    - Required API keys and environment variables
    - Customization guide

- [ ] 36. Checkpoint - Templates Complete
  - Verify all 12 templates load correctly
  - Verify templates generate valid code

---

## Updated Summary

### Total Nodes: 14 (10 core + 4 additional)

| Phase | Nodes/Deliverables | Timeline |
|-------|-------------------|----------|
| 1. Core | Trigger, HTTP, Set, Transform, Switch, Loop, Merge, Wait, Code, Database | 4-6 weeks |
| 2. Communication | Email, Notification | 2 weeks |
| 3. Data Sources | RSS/Feed, File | 2 weeks |
| 4. AI Enhancement* | Vector Search, Document Parser | 3-4 weeks |
| 5. Templates | 12 n8n-inspired workflow templates | 1-2 weeks |

*Stretch goals

### Property Tests Summary (Updated)

| # | Property | Requirements |
|---|----------|--------------|
| 1 | Standard Properties Persistence | 1.1-1.6 |
| 2 | Error Handling Mode Behavior | 1.2 |
| 3 | HTTP Variable Interpolation | 3.1 |
| 4 | Switch Condition Evaluation | 6.1, 6.2 |
| 5 | Loop Result Aggregation | 7.4 |
| 6 | Merge Wait Behavior | 8.1 |
| 7 | Code Sandbox Isolation | 10.2 |
| 8 | Database Connection Security | 11.2 |
| 9 | Action Node Visual Distinction | 12.1 |
| 10 | Code Generation Validity | 13.1 |
| 11* | Email Credential Security | 14.1, 14.2 |
| 12* | Vector Search Query Accuracy | 18.2 |

---

## n8n Workflow Coverage

With 14 action nodes, we can implement all 12 n8n example workflows:

| # | Workflow | Achievable? |
|---|----------|-------------|
| 1 | AI-Powered Lead Generation | ✅ |
| 2 | Intelligent Email Marketing | ✅ |
| 3 | Social Media AI Content Creator | ✅ |
| 4 | AI Customer Support | ✅ |
| 5 | Intelligent Data Analysis | ✅ |
| 6 | AI Invoice Processing | ✅ (with Document Parser) |
| 7 | AI Job Application Bot | ✅ |
| 8 | AI Newsletter Digest | ✅ |
| 9 | E-commerce Order Intelligence | ✅ |
| 10 | AI Incident Response | ✅ |
| 11 | AI Code Review | ✅ |
| 12 | AI Customer Onboarding | ✅ |
