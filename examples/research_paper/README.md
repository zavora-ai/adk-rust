# Research Paper Generator - Full-Stack Example

This example demonstrates a complete client-server architecture using ADK-Rust, showcasing how a frontend application can interact with a backend AI agent to conduct research and generate PDF documents.

## Business Case

**Deep Research Assistant**: An AI-powered system that conducts comprehensive research on any topic and produces professional research papers in PDF format. This demonstrates real-world integration patterns for building production AI applications.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        Frontend                              │
│  ┌────────────────────────────────────────────────────┐    │
│  │  HTML/JavaScript Client (frontend.html)            │    │
│  │  - Research topic input                            │    │
│  │  - Real-time streaming display                     │    │
│  │  - PDF download interface                          │    │
│  └────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
                            │
                            │ HTTP/SSE
                            ▼
┌─────────────────────────────────────────────────────────────┐
│                    REST API Server                           │
│  ┌────────────────────────────────────────────────────┐    │
│  │  ADK Server (Axum)                                 │    │
│  │  - POST /api/run_sse (streaming)                   │    │
│  │  - POST /api/sessions                              │    │
│  │  - GET /api/sessions/.../artifacts/...            │    │
│  └────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│                    Backend Agent                             │
│  ┌────────────────────────────────────────────────────┐    │
│  │  Research Assistant Agent                          │    │
│  │  - LLM: Gemini 2.0 Flash                          │    │
│  │  - Tools:                                          │    │
│  │    • conduct_research                              │    │
│  │    • generate_pdf                                  │    │
│  │    • format_citation                               │    │
│  └────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│                   Artifact Storage                           │
│  ┌────────────────────────────────────────────────────┐    │
│  │  InMemoryArtifactService                           │    │
│  │  - Stores generated PDFs                           │    │
│  │  - Provides download URLs                          │    │
│  └────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

## Features

### Backend (Rust/ADK)

- **Research Agent**: Intelligent agent that conducts research and synthesizes findings
- **Custom Tools**:
  - `conduct_research`: Simulates deep research with source gathering
  - `generate_pdf`: Creates PDF documents from structured content
  - `format_citation`: Formats academic citations in APA style
- **Artifact Management**: Stores and serves generated PDFs
- **Streaming Responses**: Real-time SSE streaming for live updates

### Frontend (HTML/JavaScript)

- **Interactive UI**: Clean, modern interface for research requests
- **Real-time Updates**: Displays agent responses as they stream
- **Session Management**: Automatic session creation and management
- **PDF Download**: Direct download links for generated papers
- **Example Topics**: Quick-start buttons for common research areas

## Prerequisites

- Rust 1.70 or later
- Google API Key (for Gemini model)

## Setup

1. **Set your API key**:
   ```bash
   export GOOGLE_API_KEY="your-api-key-here"
   ```

2. **Build the example**:
   ```bash
   cargo build --example research_paper
   ```

## Running the Application

### Start the Server

```bash
cargo run --example research_paper -- serve --port 8080
```

You should see:
```
🚀 ADK Server starting on http://localhost:8080
📱 Open http://localhost:8080 in your browser
```

### Access the Frontend

1. **Option 1: Use the built-in UI**
   - Open http://localhost:8080/ui/ in your browser
   - The ADK server includes a web UI

2. **Option 2: Use the custom frontend**
   - Open `examples/research_paper/frontend.html` in your browser
   - This provides a specialized research paper interface

### Generate a Research Paper

1. Enter a research topic (e.g., "Artificial Intelligence in Healthcare")
2. Select research depth (Overview, Comprehensive, or Detailed)
3. Optionally add your name as author
4. Click "Generate Research Paper"
5. Watch the agent work in real-time
6. Download the generated PDF when complete

## API Integration Examples

### JavaScript/TypeScript

```javascript
// Create a session
const sessionResponse = await fetch('http://localhost:8080/api/sessions', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
        appName: 'research_paper_generator',
        userId: 'user123',
        sessionId: 'session456'
    })
});

const session = await sessionResponse.json();

// Submit research request with streaming
const response = await fetch('http://localhost:8080/api/run_sse', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
        appName: 'research_paper_generator',
        userId: 'user123',
        sessionId: 'session456',
        newMessage: {
            role: 'user',
            parts: [{ 
                text: 'Research quantum computing and generate a paper' 
            }]
        },
        streaming: true
    })
});

// Process streaming events
const reader = response.body.getReader();
const decoder = new TextDecoder();

while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    
    const chunk = decoder.decode(value);
    const lines = chunk.split('\n');
    
    for (const line of lines) {
        if (line.startsWith('data: ')) {
            const event = JSON.parse(line.slice(6));
            console.log('Event:', event);
        }
    }
}
```

### Python

```python
import requests
import json

# Create session
session_response = requests.post(
    'http://localhost:8080/api/sessions',
    json={
        'appName': 'research_paper_generator',
        'userId': 'user123',
        'sessionId': 'session456'
    }
)
session = session_response.json()

# Submit research request
response = requests.post(
    'http://localhost:8080/api/run_sse',
    json={
        'appName': 'research_paper_generator',
        'userId': 'user123',
        'sessionId': 'session456',
        'newMessage': {
            'role': 'user',
            'parts': [{'text': 'Research quantum computing and generate a paper'}]
        },
        'streaming': True
    },
    stream=True
)

# Process streaming events
for line in response.iter_lines():
    if line:
        line_str = line.decode('utf-8')
        if line_str.startswith('data: '):
            event = json.loads(line_str[6:])
            print('Event:', event)
```

### cURL

```bash
# Create session
curl -X POST http://localhost:8080/api/sessions \
  -H "Content-Type: application/json" \
  -d '{
    "appName": "research_paper_generator",
    "userId": "user123",
    "sessionId": "session456"
  }'

# Submit research request
curl -X POST http://localhost:8080/api/run_sse \
  -H "Content-Type: application/json" \
  -d '{
    "appName": "research_paper_generator",
    "userId": "user123",
    "sessionId": "session456",
    "newMessage": {
      "role": "user",
      "parts": [{"text": "Research quantum computing and generate a paper"}]
    },
    "streaming": true
  }'

# Download generated PDF
curl -O http://localhost:8080/api/sessions/research_paper_generator/user123/session456/artifacts/quantum_computing.pdf
```

## Agent Workflow

The research assistant follows this workflow:

1. **Research Phase**
   - Receives research topic from user
   - Calls `conduct_research` tool with topic and depth
   - Gathers sources, findings, and methodology

2. **Synthesis Phase**
   - Analyzes research findings
   - Structures content into sections:
     - Executive Summary
     - Introduction
     - Literature Review
     - Key Findings
     - Methodology
     - Conclusions
     - References

3. **Citation Phase**
   - Uses `format_citation` tool for each source
   - Formats references in APA style

4. **Generation Phase**
   - Calls `generate_pdf` tool with structured content
   - Saves PDF to artifact storage
   - Returns download URL to user

## Customization

### Adding Real PDF Generation

Replace the simplified PDF generation with a real library:

```rust
use printpdf::*;

let generate_pdf_tool = FunctionTool::new(
    "generate_pdf",
    "Generate a PDF research paper",
    |args, ctx| async move {
        // Use printpdf or similar library
        let (doc, page1, layer1) = PdfDocument::new(
            "Research Paper",
            Mm(210.0),
            Mm(297.0),
            "Layer 1"
        );
        
        // Add content...
        
        doc.save(&mut BufWriter::new(File::create("paper.pdf")?))?;
        
        // Save to artifacts...
    }
);
```

### Adding Real Research APIs

Integrate with actual research APIs:

```rust
let research_tool = FunctionTool::new(
    "conduct_research",
    "Conduct research using external APIs",
    |args, _ctx| async move {
        let topic = args.get("topic")?.as_str()?;
        
        // Call Google Scholar API
        let scholar_results = reqwest::get(
            format!("https://api.scholar.google.com/search?q={}", topic)
        ).await?;
        
        // Call arXiv API
        let arxiv_results = reqwest::get(
            format!("https://export.arxiv.org/api/query?search_query={}", topic)
        ).await?;
        
        // Aggregate and return results
        Ok(json!({ "sources": [...] }))
    }
);
```

### Adding Authentication

Add JWT authentication to the server:

```rust
use axum::middleware;

let app = Router::new()
    .route("/api/run_sse", post(run_sse))
    .layer(middleware::from_fn(auth_middleware));

async fn auth_middleware(
    req: Request<Body>,
    next: Next<Body>,
) -> Result<Response, StatusCode> {
    // Verify JWT token
    let token = req.headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok())?;
    
    verify_token(token)?;
    
    Ok(next.run(req).await)
}
```

## Production Considerations

1. **PDF Generation**: Use a proper PDF library like `printpdf` or `lopdf`
2. **Research APIs**: Integrate with real research databases (Google Scholar, arXiv, PubMed)
3. **Authentication**: Add JWT or OAuth for user authentication
4. **Rate Limiting**: Implement rate limiting to prevent abuse
5. **Persistent Storage**: Use `DatabaseSessionService` and cloud storage for artifacts
6. **Error Handling**: Add comprehensive error handling and retry logic
7. **Monitoring**: Add telemetry and logging for production monitoring
8. **CORS**: Configure CORS properly for your frontend domain
9. **HTTPS**: Use TLS certificates for secure communication
10. **Caching**: Cache research results to reduce API calls

## Troubleshooting

### Server won't start
- Check that port 8080 is not in use
- Verify GOOGLE_API_KEY is set
- Check Rust version (1.70+)

### Frontend can't connect
- Ensure server is running on http://localhost:8080
- Check browser console for CORS errors
- Verify API_BASE URL in frontend.html

### PDF not generating
- Check artifact service is configured
- Verify tool execution in server logs
- Check session ID is valid

## Related Examples

- [Console Mode](../deployment/console_mode.rs) - Running agents in console
- [Server Mode](../deployment/server_mode.rs) - Basic server setup
- [Function Tools](../tools/function_tool.rs) - Creating custom tools
- [Artifacts](../artifacts/artifact_ops.rs) - Working with artifacts

## License

This example is part of the ADK-Rust project and follows the same license.
