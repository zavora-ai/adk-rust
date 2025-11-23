# A2A Protocol Implementation Analysis

## Overview
Analysis of A2A (Agent-to-Agent) protocol implementations in Rust and Go to guide Task 7.5 implementation.

## Sources Analyzed
1. **a2a-rs** - Rust implementation by EmilLindfors (https://github.com/EmilLindfors/a2a-rs)
2. **adk-go/server/adka2a** - Google ADK Go implementation

## A2A Protocol Core Concepts

### 1. AgentCard
The primary descriptor for an agent containing:
- **Metadata**: name, description, version, provider
- **Protocol Info**: protocol version (v0.3.0), preferred transport
- **Capabilities**: streaming, push notifications, state history, extensions
- **Skills**: List of agent capabilities with tags and examples
- **Security**: Authentication schemes (OAuth, API keys, JWT)
- **Interfaces**: Multiple transport endpoints (HTTP, gRPC, WebSocket)

### 2. AgentSkill
Describes specific capabilities:
```rust
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub examples: Option<Vec<String>>,
    pub input_modes: Option<Vec<String>>,
    pub output_modes: Option<Vec<String>>,
    pub security: Option<Vec<HashMap<String, Vec<String>>>>,
}
```

### 3. Message
Primary communication unit:
```rust
pub struct Message {
    pub role: Role,  // User or Agent
    pub parts: Vec<Part>,
    pub message_id: String,
    pub task_id: Option<String>,
    pub context_id: Option<String>,
    pub metadata: Option<Map<String, Value>>,
    pub extensions: Option<Vec<String>>,
}
```

### 4. Part (Content Types)
```rust
pub enum Part {
    Text { text: String, metadata: Option<Map<String, Value>> },
    File { file: FileContent, metadata: Option<Map<String, Value>> },
    Data { data: Map<String, Value>, metadata: Option<Map<String, Value>> },
}
```

### 5. Task
Represents work to be done:
```rust
pub struct Task {
    pub task_id: String,
    pub status: TaskStatus,
    pub state: TaskState,
    pub messages: Vec<Message>,
    pub artifacts: Option<Vec<Artifact>>,
    pub metadata: Option<Map<String, Value>>,
}
```

## Go ADK Implementation (adk-go/server/adka2a/)

### Key Files:
1. **agent_card.go** - Generates AgentCard from ADK agents
2. **events.go** - Converts ADK events to A2A events
3. **parts.go** - Converts between ADK and A2A parts
4. **processor.go** - Processes A2A requests
5. **executor.go** - Executes A2A tasks

### Skill Extraction Logic (agent_card.go)

#### For LLM Agents:
```go
func buildLLMAgentSkills(agent agent.Agent, llmState *llminternal.State) []a2a.AgentSkill {
    skills := []a2a.AgentSkill{
        {
            ID:          agent.Name(),
            Name:        "model",
            Description: buildDescriptionFromInstructions(agent, llmState),
            Tags:        []string{"llm"},
        },
    }
    
    // Add tool skills
    for _, tool := range llmState.Tools {
        skills = append(skills, a2a.AgentSkill{
            ID:          fmt.Sprintf("%s-%s", agent.Name(), tool.Name()),
            Name:        tool.Name(),
            Description: tool.Description(),
            Tags:        []string{"llm", "tools"},
        })
    }
    
    return skills
}
```

#### For Workflow Agents:
```go
func buildNonLLMAgentSkills(agent agent.Agent) []a2a.AgentSkill {
    skills := []a2a.AgentSkill{
        {
            ID:          agent.Name(),
            Name:        getAgentSkillName(state),
            Description: buildAgentDescription(agent, state),
            Tags:        []string{getAgentTypeTag(state)},
        },
    }
    
    // Add sub-agent orchestration skill
    if len(agent.SubAgents()) > 0 {
        skills = append(skills, a2a.AgentSkill{
            ID:          fmt.Sprintf("%s-sub-agents", agent.Name()),
            Name:        "sub-agents",
            Description: fmt.Sprintf("Orchestrates: %s", ...),
            Tags:        []string{getAgentTypeTag(state), "orchestration"},
        })
    }
    
    return skills
}
```

#### Recursive Sub-Agent Processing:
```go
func buildSubAgentSkills(agent agent.Agent) []a2a.AgentSkill {
    result := []a2a.AgentSkill{}
    for _, sub := range agent.SubAgents() {
        skills := buildPrimarySkills(sub)
        for _, subSkill := range skills {
            skill := a2a.AgentSkill{
                ID:          fmt.Sprintf("%s_%s", sub.Name(), subSkill.ID),
                Name:        fmt.Sprintf("%s: %s", sub.Name(), subSkill.Name),
                Description: subSkill.Description,
                Tags:        append([]string{fmt.Sprintf("sub_agent:%s", sub.Name())}, subSkill.Tags...),
            }
            result = append(result, skill)
        }
    }
    return result
}
```

### Part Conversion (parts.go)

#### ADK Content → A2A Parts:
```go
func ContentToParts(content *genai.Content) []a2a.Part {
    parts := []a2a.Part{}
    for _, part := range content.Parts {
        switch p := part.(type) {
        case genai.Text:
            parts = append(parts, a2a.Part{Text: string(p)})
        case genai.Blob:
            parts = append(parts, a2a.Part{
                File: &a2a.FileContent{
                    MimeType: p.MIMEType,
                    Bytes:    base64.StdEncoding.EncodeToString(p.Data),
                },
            })
        case genai.FileData:
            parts = append(parts, a2a.Part{
                File: &a2a.FileContent{
                    MimeType: p.MIMEType,
                    URI:      p.FileURI,
                },
            })
        }
    }
    return parts
}
```

### Event Conversion (events.go)

#### ADK Event → A2A Message:
```go
func EventToMessage(event *session.Event) a2a.Message {
    parts := ContentToParts(event.UserContent)
    
    return a2a.Message{
        Role:      a2a.RoleUser,
        Parts:     parts,
        MessageID: event.InvocationID,
        TaskID:    event.SessionID,
        ContextID: event.SessionID,
    }
}
```

## Rust a2a-rs Implementation

### Architecture
Uses hexagonal architecture:
- **Domain**: Core types (AgentCard, Message, Task, etc.)
- **Port**: Traits for handlers (MessageHandler, TaskManager, etc.)
- **Adapter**: HTTP/WebSocket clients and servers
- **Application**: Business logic and services

### Key Traits:

#### MessageHandler:
```rust
#[async_trait]
pub trait AsyncMessageHandler: Send + Sync {
    async fn handle_message(
        &self,
        task_id: &str,
        message: &Message,
        config: Option<&MessageSendConfiguration>,
    ) -> Result<Message, A2AError>;
}
```

#### TaskManager:
```rust
#[async_trait]
pub trait AsyncTaskManager: Send + Sync {
    async fn create_task(&self, task: Task) -> Result<Task, A2AError>;
    async fn get_task(&self, task_id: &str) -> Result<Task, A2AError>;
    async fn update_task(&self, task: Task) -> Result<Task, A2AError>;
    async fn list_tasks(&self, params: &ListTasksParams) -> Result<ListTasksResult, A2AError>;
}
```

### Builder Patterns:
```rust
let card = AgentCard::builder()
    .name("My Agent".to_string())
    .description("A helpful AI agent".to_string())
    .url("https://agent.example.com".to_string())
    .version("1.0.0".to_string())
    .capabilities(AgentCapabilities::default())
    .skills(vec![
        AgentSkill::new(
            "text-generation".to_string(),
            "Text Generation".to_string(),
            "Generate natural language text".to_string(),
            vec!["nlp".to_string()],
        ),
    ])
    .build();
```

## Implementation Strategy for ADK-Rust Task 7.5

### Option 1: Use a2a-rs as Dependency
**Pros:**
- Complete, tested implementation
- Follows A2A spec v0.3.0
- Active maintenance
- Hexagonal architecture

**Cons:**
- Large dependency (~20+ files)
- May include features we don't need
- Need to adapt to ADK patterns

### Option 2: Minimal Custom Implementation
**Pros:**
- Only what we need
- Tight integration with ADK types
- Smaller footprint
- Full control

**Cons:**
- More work
- Need to maintain spec compliance
- Potential bugs

### Recommended Approach: Hybrid

1. **Use a2a-rs for core types** (AgentCard, Message, Part, etc.)
2. **Implement ADK-specific conversion layer**:
   - `adk-server/src/a2a/converter.rs` - Convert ADK types to A2A types
   - `adk-server/src/a2a/skill_extractor.rs` - Extract skills from agents
   - `adk-server/src/a2a/card_builder.rs` - Build AgentCard from ADK agents

### Minimal Implementation Plan

#### Phase 1: Core Types (Use a2a-rs)
Add dependency:
```toml
[dependencies]
a2a-rs = { version = "0.3", default-features = false, features = ["domain"] }
```

#### Phase 2: Conversion Layer
```rust
// adk-server/src/a2a/converter.rs
pub fn adk_content_to_a2a_parts(content: &adk_core::Content) -> Vec<a2a_rs::Part> {
    content.parts.iter().map(|part| match part {
        adk_core::Part::Text { text } => a2a_rs::Part::text(text.clone()),
        adk_core::Part::InlineData { mime_type, data } => {
            a2a_rs::Part::file(a2a_rs::FileContent {
                mime_type: Some(mime_type.clone()),
                bytes: Some(base64::encode(data)),
                uri: None,
                name: None,
            })
        }
        // ... other conversions
    }).collect()
}

pub fn adk_event_to_a2a_message(event: &adk_core::Event) -> a2a_rs::Message {
    a2a_rs::Message::builder()
        .role(a2a_rs::Role::User)
        .parts(adk_content_to_a2a_parts(&event.user_content))
        .message_id(event.invocation_id.clone())
        .task_id(Some(event.session_id.clone()))
        .build()
}
```

#### Phase 3: Skill Extraction
```rust
// adk-server/src/a2a/skill_extractor.rs
pub fn extract_skills(agent: &dyn adk_core::Agent) -> Vec<a2a_rs::AgentSkill> {
    let mut skills = vec![];
    
    // Primary skill
    skills.push(a2a_rs::AgentSkill::new(
        agent.name().to_string(),
        agent.name().to_string(),
        agent.description().to_string(),
        vec!["agent".to_string()],
    ));
    
    // Sub-agent skills (recursive)
    for sub in agent.sub_agents() {
        let sub_skills = extract_skills(sub.as_ref());
        for skill in sub_skills {
            skills.push(a2a_rs::AgentSkill::new(
                format!("{}_{}", sub.name(), skill.id),
                format!("{}: {}", sub.name(), skill.name),
                skill.description,
                skill.tags,
            ));
        }
    }
    
    skills
}
```

#### Phase 4: AgentCard Builder
```rust
// adk-server/src/a2a/card_builder.rs
pub fn build_agent_card(
    agent: &dyn adk_core::Agent,
    base_url: &str,
) -> a2a_rs::AgentCard {
    a2a_rs::AgentCard::builder()
        .name(agent.name().to_string())
        .description(agent.description().to_string())
        .url(base_url.to_string())
        .version("1.0.0".to_string())
        .capabilities(a2a_rs::AgentCapabilities {
            streaming: true,
            push_notifications: false,
            state_transition_history: true,
            extensions: None,
        })
        .skills(extract_skills(agent))
        .build()
}
```

#### Phase 5: REST Endpoint
```rust
// adk-server/src/rest/controllers/a2a.rs
pub async fn get_agent_card(
    State(controller): State<A2AController>,
    Path(app_name): Path<String>,
) -> Result<Json<a2a_rs::AgentCard>, StatusCode> {
    let agent = controller
        .config
        .agent_loader
        .load_agent(&app_name)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    
    let card = build_agent_card(agent.as_ref(), &controller.base_url);
    Ok(Json(card))
}
```

## Minimal Scope for Task 7.5

### Must Have:
1. ✅ AgentCard generation endpoint: `GET /a2a/agents/:app_name/card`
2. ✅ Skill extraction from ADK agents
3. ✅ Basic Part conversion (Text, InlineData)

### Nice to Have (Defer):
- Full Message/Task handling
- WebSocket support
- Push notifications
- Streaming events
- Complete security schemes

### Estimated Effort:
- **With a2a-rs dependency**: 2-3 hours
- **Custom minimal implementation**: 4-5 hours

## Recommendation

**Use a2a-rs with minimal feature set:**
1. Add `a2a-rs` dependency with only `domain` feature
2. Implement conversion layer (3 files, ~200 lines total)
3. Add single endpoint for AgentCard
4. Write basic tests

This gives us:
- Spec compliance
- Minimal code to maintain
- Foundation for future A2A features
- Quick implementation
