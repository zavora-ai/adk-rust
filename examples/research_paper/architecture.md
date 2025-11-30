# Research Paper Generator - Architecture

## System Overview

```mermaid
graph TB
    subgraph "Frontend Layer"
        UI[Web Browser]
        HTML[frontend.html]
        JS[JavaScript Client]
    end
    
    subgraph "API Layer"
        Server[ADK Server<br/>Axum HTTP]
        SSE[SSE Streaming]
        REST[REST Endpoints]
    end
    
    subgraph "Agent Layer"
        Agent[Research Assistant<br/>LlmAgent]
        Model[Gemini 2.0 Flash]
    end
    
    subgraph "Tools Layer"
        T1[conduct_research]
        T2[generate_pdf]
        T3[format_citation]
    end
    
    subgraph "Storage Layer"
        Artifacts[Artifact Service]
        Sessions[Session Service]
    end
    
    UI --> HTML
    HTML --> JS
    JS -->|HTTP POST| REST
    JS -->|SSE Stream| SSE
    REST --> Server
    SSE --> Server
    Server --> Agent
    Agent --> Model
    Agent --> T1
    Agent --> T2
    Agent --> T3
    T2 --> Artifacts
    Server --> Sessions
    Artifacts -->|Download URL| JS
```

## Request Flow

### 1. Session Creation

```mermaid
sequenceDiagram
    participant Browser
    participant API
    participant SessionService
    
    Browser->>API: POST /api/sessions
    API->>SessionService: create(CreateRequest)
    SessionService-->>API: Session
    API-->>Browser: {id, appName, userId}
```

### 2. Research Request

```mermaid
sequenceDiagram
    participant Browser
    participant API
    participant Agent
    participant Tools
    participant Artifacts
    
    Browser->>API: POST /api/run_sse<br/>{topic, depth}
    API->>Agent: run(message)
    
    loop Research Phase
        Agent->>Tools: conduct_research(topic)
        Tools-->>Agent: findings
        Agent-->>Browser: SSE: research findings
    end
    
    loop Synthesis Phase
        Agent->>Agent: analyze & structure
        Agent-->>Browser: SSE: content sections
    end
    
    loop Citation Phase
        Agent->>Tools: format_citation(source)
        Tools-->>Agent: formatted citation
        Agent-->>Browser: SSE: references
    end
    
    Agent->>Tools: generate_pdf(content)
    Tools->>Artifacts: save(pdf)
    Artifacts-->>Tools: download_url
    Tools-->>Agent: {status, url}
    Agent-->>Browser: SSE: download link
```

### 3. PDF Download

```mermaid
sequenceDiagram
    participant Browser
    participant API
    participant Artifacts
    
    Browser->>API: GET /api/sessions/.../artifacts/paper.pdf
    API->>Artifacts: load(LoadRequest)
    Artifacts-->>API: Part::Text{pdf_content}
    API-->>Browser: application/pdf
```

## Component Details

### Frontend Components

```
frontend.html
├── UI Components
│   ├── Research Form
│   │   ├── Topic Input
│   │   ├── Depth Selector
│   │   └── Author Input
│   ├── Status Display
│   └── Output Area
├── Session Manager
│   └── Auto-initialization
├── SSE Client
│   ├── Event Parser
│   └── Stream Handler
└── Download Manager
    └── URL Handler
```

### Backend Components

```
main.rs
├── Agent Configuration
│   ├── LlmAgentBuilder
│   ├── Model Setup
│   └── Instruction Template
├── Tool Definitions
│   ├── conduct_research
│   │   ├── Topic Analysis
│   │   ├── Source Gathering
│   │   └── Findings Synthesis
│   ├── generate_pdf
│   │   ├── Content Formatting
│   │   ├── PDF Creation
│   │   └── Artifact Storage
│   └── format_citation
│       └── APA Formatting
└── Launcher
    ├── Server Mode
    └── Artifact Service
```

## Data Flow

### Research Data Structure

```json
{
  "topic": "Quantum Computing",
  "depth": "comprehensive",
  "sources": [
    {
      "title": "Recent Advances in Quantum Computing",
      "summary": "...",
      "relevance": "high"
    }
  ],
  "key_findings": [
    "Finding 1",
    "Finding 2"
  ],
  "methodology": "Literature review",
  "timestamp": "2024-01-15T10:30:00Z"
}
```

### PDF Generation Request

```json
{
  "title": "Quantum Computing Research Paper",
  "content": "# Executive Summary\n\n...",
  "author": "Research Assistant"
}
```

### PDF Generation Response

```json
{
  "status": "success",
  "filename": "quantum_computing_research_paper.pdf",
  "size_bytes": 15420,
  "download_url": "/api/sessions/.../artifacts/quantum_computing_research_paper.pdf",
  "message": "PDF generated successfully"
}
```

## Scalability Considerations

### Current Architecture (Development)

- **Session Storage**: In-memory (InMemorySessionService)
- **Artifact Storage**: In-memory (InMemoryArtifactService)
- **Concurrency**: Single server instance
- **State**: Ephemeral (lost on restart)

### Production Architecture

```mermaid
graph TB
    subgraph "Load Balancer"
        LB[Nginx/ALB]
    end
    
    subgraph "Application Tier"
        S1[ADK Server 1]
        S2[ADK Server 2]
        S3[ADK Server N]
    end
    
    subgraph "Data Tier"
        DB[(PostgreSQL<br/>Sessions)]
        S3Storage[S3/GCS<br/>Artifacts]
        Redis[(Redis<br/>Cache)]
    end
    
    LB --> S1
    LB --> S2
    LB --> S3
    S1 --> DB
    S2 --> DB
    S3 --> DB
    S1 --> S3Storage
    S2 --> S3Storage
    S3 --> S3Storage
    S1 --> Redis
    S2 --> Redis
    S3 --> Redis
```

### Recommended Production Changes

1. **Session Storage**: Use `DatabaseSessionService` with PostgreSQL
2. **Artifact Storage**: Use GCS or S3 for PDF storage
3. **Caching**: Add Redis for session caching
4. **Load Balancing**: Multiple server instances behind load balancer
5. **Authentication**: Add JWT/OAuth for user authentication
6. **Rate Limiting**: Implement per-user rate limits
7. **Monitoring**: Add Prometheus metrics and distributed tracing
8. **CDN**: Serve static frontend assets via CDN

## Security Considerations

### Current Implementation

- ✅ CORS enabled (permissive for development)
- ✅ Input validation in tools
- ✅ Error handling
- ❌ No authentication
- ❌ No rate limiting
- ❌ No input sanitization for PDF content

### Production Requirements

1. **Authentication**: JWT tokens or OAuth 2.0
2. **Authorization**: Role-based access control
3. **Rate Limiting**: Per-user and per-IP limits
4. **Input Validation**: Sanitize all user inputs
5. **CORS**: Restrict to specific domains
6. **HTTPS**: TLS certificates required
7. **API Keys**: Secure storage (not in code)
8. **Audit Logging**: Track all research requests

## Performance Optimization

### Current Bottlenecks

1. **LLM Latency**: Gemini API calls (2-5 seconds)
2. **PDF Generation**: Synchronous processing
3. **Memory**: In-memory storage limits

### Optimization Strategies

1. **Streaming**: Already implemented via SSE
2. **Caching**: Cache research results for common topics
3. **Async Processing**: Queue PDF generation jobs
4. **Compression**: Compress large PDFs
5. **CDN**: Cache static assets
6. **Connection Pooling**: Reuse HTTP connections
7. **Batch Processing**: Group multiple research requests

## Monitoring & Observability

### Key Metrics

- Request rate (requests/second)
- Response time (p50, p95, p99)
- Error rate (%)
- Active sessions
- PDF generation time
- Artifact storage size
- LLM token usage

### Logging

```rust
// Add structured logging
tracing::info!(
    topic = %topic,
    depth = %depth,
    user_id = %user_id,
    "Research request received"
);

tracing::info!(
    filename = %filename,
    size_bytes = %size,
    duration_ms = %duration,
    "PDF generated successfully"
);
```

### Tracing

Enable distributed tracing with OpenTelemetry:

```rust
use adk_telemetry::init_telemetry;

init_telemetry("research_paper_generator")?;
```

## Testing Strategy

### Unit Tests

- Tool function logic
- PDF generation
- Citation formatting
- Input validation

### Integration Tests

- API endpoint responses
- Session management
- Artifact storage/retrieval
- SSE streaming

### End-to-End Tests

- Full research workflow
- Frontend-backend integration
- PDF download
- Error scenarios

### Load Tests

- Concurrent users
- Large research requests
- Artifact storage limits
- Memory usage under load
