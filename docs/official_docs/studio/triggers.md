# Triggers

Triggers define how workflows start in ADK Studio. Every workflow begins with a Trigger node that determines the entry point — whether from user input, an HTTP webhook, a cron schedule, or an external event.

![Trigger Properties Panel](images/09_trigger_properties.png)

## Trigger Types

| Type | Description | Use Case |
|------|-------------|----------|
| **Manual** | User-initiated via chat input | Interactive agents, testing |
| **Webhook** | HTTP endpoint (POST/GET) | API integrations, CI/CD pipelines |
| **Schedule** | Cron-based timing | Periodic reports, data sync |
| **Event** | External system events | Microservice orchestration, event-driven workflows |

---

## Manual Triggers

The default trigger type. When a workflow has a manual trigger, the chat input is shown with a configurable label and placeholder.

**Configuration:**

| Property | Type | Default |
|----------|------|---------|
| `inputLabel` | string | "Enter your message" |
| `defaultPrompt` | string | "What can you help me build with ADK-Rust today?" |

The input label appears above the chat input field, and the default prompt is used as placeholder text.

---

## Webhook Triggers

Expose an HTTP endpoint that starts the workflow when called. Useful for integrating with external services, CI/CD pipelines, or other applications.

**Configuration:**

| Property | Type | Description |
|----------|------|-------------|
| `path` | string | URL path (e.g., `/my-webhook`) |
| `method` | GET, POST | HTTP method to accept |
| `auth` | none, bearer, api_key | Authentication requirement |

### Endpoints

Each webhook trigger creates two endpoints:

| Endpoint | Behavior |
|----------|----------|
| `/api/projects/:id/webhook/*path` | Async — returns session ID immediately |
| `/api/projects/:id/webhook-exec/*path` | Sync — waits for workflow completion and returns result |

GET webhooks also work:

```
GET /api/projects/:id/webhook/my-path?message=Hello
```

### Authentication

**Bearer token:**
```bash
curl -X POST "http://localhost:3000/api/projects/{id}/webhook/my-path" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"message": "Process this"}'
```

**API key:**
```bash
curl -X POST "http://localhost:3000/api/projects/{id}/webhook/my-path" \
  -H "X-API-Key: $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"message": "Process this"}'
```

### Webhook Event Notifications

Subscribe to real-time webhook execution events via SSE:

```
GET /api/projects/:id/webhook-events
```

This stream emits events as webhooks are received and processed.

---

## Schedule Triggers

Run workflows on a recurring schedule using cron expressions.

**Configuration:**

| Property | Type | Description |
|----------|------|-------------|
| `cron` | string | 5-field cron expression |
| `timezone` | string | IANA timezone (e.g., `America/New_York`) |
| `defaultPrompt` | string? | Input text sent when schedule fires |

### Cron Syntax

Standard 5-field cron expressions:

```
┌───────────── minute (0-59)
│ ┌───────────── hour (0-23)
│ │ ┌───────────── day of month (1-31)
│ │ │ ┌───────────── month (1-12)
│ │ │ │ ┌───────────── day of week (0-6, Sun=0)
│ │ │ │ │
* * * * *
```

**Examples:**

| Expression | Meaning |
|-----------|---------|
| `* * * * *` | Every minute |
| `0 9 * * *` | Daily at 9:00 AM |
| `0 0 * * 0` | Weekly on Sunday at midnight |
| `0 */6 * * *` | Every 6 hours |
| `30 8 * * 1-5` | Weekdays at 8:30 AM |

The schedule service tracks `last_executed` timestamps to prevent duplicate runs.

---

## Event Triggers

Start workflows in response to external system events. Events are matched by `source` and `eventType`, with optional JSONPath filtering on the event data.

**Configuration:**

| Property | Type | Description |
|----------|------|-------------|
| `source` | string | Event source identifier (e.g., `payment-service`) |
| `eventType` | string | Event type to match (e.g., `payment.completed`) |
| `filter` | string? | JSONPath expression to filter events |

### Sending Events

```bash
curl -X POST "http://localhost:3000/api/projects/{id}/events" \
  -H "Content-Type: application/json" \
  -d '{
    "source": "payment-service",
    "eventType": "payment.completed",
    "data": {
      "orderId": "12345",
      "amount": 99.00,
      "status": "active"
    }
  }'
```

### JSONPath Filters

Filter events so the workflow only triggers when specific conditions are met:

| Filter Expression | Matches When |
|-------------------|-------------|
| `$.data.status == 'active'` | Status field equals "active" |
| `$.data.amount > 100` | Amount exceeds 100 |
| `$.data.priority == 'high'` | Priority is "high" |

Events that don't match the filter are silently ignored.

---

## Trigger-Aware Run Button

The Studio UI adapts based on the active trigger type:

- **Manual** — Standard chat input with the configured label and placeholder
- **Webhook** — Run button sends a simulated webhook payload
- **Schedule** — Run button sends the configured default prompt
- **Event** — Run button sends a simulated event payload

When a workflow has changed since the last build, the Send button transforms into a Build button to prompt recompilation.

---

## API Reference

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/projects/:id/webhook/*path` | POST, GET | Async webhook trigger |
| `/api/projects/:id/webhook-exec/*path` | POST | Sync webhook trigger (waits for result) |
| `/api/projects/:id/webhook-events` | GET | SSE stream for webhook notifications |
| `/api/projects/:id/events` | POST | Event trigger |

---

**Previous**: [← Action Nodes](action-nodes.md) | **Next**: [Development Guidelines →](../development/development-guidelines.md)
