# MultiAgentLoader Implementation Plan

## Go Implementation Analysis

### Key Features
1. **MultiLoader** - Manages multiple agents by name
   - `agentMap: map[string]Agent` - HashMap of agent name → agent
   - `root: Agent` - Default/root agent
   - `NewMultiLoader(root, agents...)` - Constructor with root + variadic agents
   - `ListAgents()` - Returns all agent names
   - `LoadAgent(name)` - Returns agent by name or error
   - `RootAgent()` - Returns root agent

2. **SingleLoader** - Single agent (already implemented in Rust)
   - `root: Agent` - The only agent
   - Returns root for empty name or root.Name()
   - Error for other names

### Go Usage Pattern
```go
agentLoader, err := agent.NewMultiLoader(
    rootAgent,
    llmAuditor,
    imageGeneratorAgent,
)
```

## Current Rust Implementation

### AgentLoader Trait
```rust
#[async_trait]
pub trait AgentLoader: Send + Sync {
    async fn load_agent(&self, app_name: &str) -> Result<Arc<dyn Agent>>;
}
```

**Issue**: Current trait only has `load_agent(app_name)`, but Go uses agent **name** not app_name.

### SingleAgentLoader
- ✅ Already implemented
- Returns same agent for all app names

## Implementation Plan

### 1. Update AgentLoader Trait (~10 lines)
Add methods to match Go interface:
```rust
#[async_trait]
pub trait AgentLoader: Send + Sync {
    async fn load_agent(&self, app_name: &str) -> Result<Arc<dyn Agent>>;
    fn list_agents(&self) -> Vec<String>;
    fn root_agent(&self) -> Arc<dyn Agent>;
}
```

### 2. Update SingleAgentLoader (~5 lines)
Implement new trait methods:
```rust
impl AgentLoader for SingleAgentLoader {
    async fn load_agent(&self, _app_name: &str) -> Result<Arc<dyn Agent>> {
        Ok(self.agent.clone())
    }
    
    fn list_agents(&self) -> Vec<String> {
        vec![self.agent.name().to_string()]
    }
    
    fn root_agent(&self) -> Arc<dyn Agent> {
        self.agent.clone()
    }
}
```

### 3. Implement MultiAgentLoader (~40 lines)
```rust
pub struct MultiAgentLoader {
    agent_map: HashMap<String, Arc<dyn Agent>>,
    root: Arc<dyn Agent>,
}

impl MultiAgentLoader {
    pub fn new(agents: Vec<Arc<dyn Agent>>) -> Result<Self> {
        // First agent is root
        // Build HashMap checking for duplicates
        // Return error if duplicate names found
    }
}

#[async_trait]
impl AgentLoader for MultiAgentLoader {
    async fn load_agent(&self, app_name: &str) -> Result<Arc<dyn Agent>> {
        // If empty or root name, return root
        // Otherwise lookup in map
    }
    
    fn list_agents(&self) -> Vec<String> {
        self.agent_map.keys().cloned().collect()
    }
    
    fn root_agent(&self) -> Arc<dyn Agent> {
        self.root.clone()
    }
}
```

### 4. Export from adk-core (~1 line)
```rust
pub use agent_loader::{AgentLoader, SingleAgentLoader, MultiAgentLoader};
```

### 5. Update web.rs Example (~5 lines)
```rust
let agent_loader = Arc::new(MultiAgentLoader::new(vec![
    Arc::new(weather_agent),
    Arc::new(research_agent),
    Arc::new(summary_agent),
])?);
```

## Design Decisions

### Agent Selection Strategy
**Go approach**: Uses agent name for routing
- `LoadAgent("weather_agent")` → returns weather agent
- Empty name → returns root agent

**Rust approach**: Keep same pattern
- First agent in vec becomes root
- Load by agent name, not app_name
- Empty/root name → root agent

### Error Handling
- Duplicate names → Error at construction time
- Agent not found → Error at load time with helpful message listing available agents

## Total Implementation

- **Lines**: ~60 lines
- **Files**: 2 (agent_loader.rs, lib.rs)
- **Time**: 15 minutes
- **Breaking changes**: Yes (trait signature change)

## Migration Impact

### Existing Code
All existing code uses `SingleAgentLoader` which will be updated to implement new trait methods.

### Server/CLI Code
May need updates if they call `load_agent()` - need to check usage patterns.

## Next Steps

1. ✅ Review Go implementation
2. ✅ Create plan
3. Implement MultiAgentLoader in adk-core
4. Update SingleAgentLoader with new methods
5. Update trait definition
6. Test with web.rs example
7. Check for breaking changes in server/CLI
