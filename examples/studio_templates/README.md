# ADK Studio Example Templates

15 practical, ready-to-use agent workflow templates for ADK Studio. Each template solves a real-world use case and can be imported directly.

## Templates

| Template | Use Case | Key Nodes |
|----------|----------|-----------|
| **Customer Onboarding** | Welcome email, data enrichment, CRM update | Trigger → HTTP → Set → LLM → HTTP |
| **Content Moderation** | Classify content, flag violations, auto-respond | Trigger → LLM → Switch → Merge → LLM |
| **Daily Standup Digest** | Pull Jira + Slack, summarize with LLM | Trigger → HTTP ×2 → Transform → LLM → HTTP |
| **Lead Scoring & Routing** | Score leads, route to sales or nurture | Trigger → HTTP → LLM → Switch → Merge → DB |
| **Incident Response Bot** | Classify severity, page on-call, post status | Trigger → LLM → Switch → HTTP/Set → Merge → LLM |
| **Invoice Processing** | Extract data, validate, route for approval | Trigger → LLM → Transform → Switch → Merge → HTTP |
| **Employee Offboarding** | Revoke access, checklist, notify teams | Trigger → HTTP ×2 → Set → LLM → HTTP → LLM |
| **Bug Triage Bot** | Classify bugs, assign teams, create tickets | Trigger → LLM → HTTP → Switch → Merge → HTTP ×2 |
| **Newsletter Generator** | Multi-source fetch, AI curation, send | Trigger → HTTP ×3 → Transform → LLM ×2 → HTTP → DB |
| **Data Pipeline Monitor** | Diagnose ETL failures, auto-retry, alert | Trigger → HTTP ×2 → LLM → Switch → Merge → HTTP → DB |
| **Contract Reviewer** | Extract clauses, flag risks, route to legal | Trigger → LLM ×2 → Switch → Merge → DB → HTTP |
| **Social Media Scheduler** | Generate platform posts, publish, track | Trigger → LLM → HTTP ×2 + Set → Merge → DB → HTTP |
| **Expense Report Processor** | Extract receipts, check policy, approve/reject | Trigger → LLM ×2 → Switch → Merge → DB → HTTP |
| **Customer Churn Predictor** | Score churn risk, trigger retention actions | Trigger → HTTP ×2 → LLM → Switch → Merge → DB → HTTP |
| **API Health Dashboard** | Monitor endpoints, diagnose, alert on issues | Trigger → HTTP ×5 → Transform → LLM → Switch → Merge → HTTP → DB |

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
