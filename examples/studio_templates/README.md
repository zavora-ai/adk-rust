# ADK Studio Example Templates

Ten practical, ready-to-use agent workflow templates for ADK Studio. Each template solves a real-world use case and can be imported directly.

## Templates

| Template | Use Case | Nodes |
|----------|----------|-------|
| **Customer Onboarding** | Automated welcome email, data enrichment, CRM update | Trigger → HTTP → Set → LLM → HTTP → END |
| **Content Moderation Pipeline** | Classify user content, flag violations, auto-respond | Trigger → LLM → Switch → Set/HTTP branches → Merge → END |
| **Daily Standup Digest** | Pull Jira tickets + Slack messages, summarize with LLM | Trigger → HTTP (×2) → Transform → LLM → HTTP → END |
| **Lead Scoring & Routing** | Score inbound leads, route to sales or nurture | Trigger → HTTP → LLM → Switch → Set branches → Merge → Database → END |
| **Incident Response Bot** | Detect severity, page on-call, create ticket, post status | Trigger → LLM → Switch → HTTP/Set branches → Merge → LLM → HTTP → END |
| **Invoice Processing** | Extract invoice data with AI, validate, route for approval | Trigger → LLM → Transform → Switch → Set branches → Merge → HTTP → END |
| **Employee Offboarding** | Revoke access, generate checklist, notify teams | Trigger → HTTP (×2 parallel) → Set → LLM → HTTP Jira → LLM → HTTP Slack → END |
| **Bug Triage Bot** | Classify bugs, assign to teams, create tickets | Trigger → LLM → HTTP dup-check → Switch → Set branches → Merge → HTTP Jira → HTTP Slack → END |
| **Newsletter Generator** | Pull from HN/PH/RSS, curate with AI, draft and send | Trigger → HTTP (×3 parallel) → Transform → LLM → LLM → HTTP SendGrid → Database → END |
| **Data Pipeline Monitor** | Diagnose ETL failures, auto-retry or escalate | Trigger → HTTP (×2 parallel) → LLM → Switch → HTTP retry/Set branches → Merge → HTTP Slack → Database → END |

## How to Use

1. Open ADK Studio (`cargo run -p adk-studio -- --port 6000`)
2. Copy any `.json` file to `~/Library/Application Support/adk-studio/projects/`
3. Refresh the project list — the template will appear
4. Open the project, review the workflow, and click **Build**

Alternatively, use these as reference when building your own workflows from the Templates gallery inside ADK Studio.

## Requirements

- ADK Studio v0.2.4+
- A Gemini API key set in your environment (`GEMINI_API_KEY`)
- For HTTP nodes: the target APIs must be accessible (or replace URLs with your own)
