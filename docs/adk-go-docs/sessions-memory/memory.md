Memory: Long-Term Knowledge with MemoryService¶
Supported in ADKPython v0.1.0Go v0.1.0Java v0.2.0
We've seen how Session tracks the history (events) and temporary data (state) for a single, ongoing conversation. But what if an agent needs to recall information from past conversations? This is where the concept of Long-Term Knowledge and the MemoryService come into play.

Think of it this way:

Session / State: Like your short-term memory during one specific chat.
Long-Term Knowledge (MemoryService): Like a searchable archive or knowledge library the agent can consult, potentially containing information from many past chats or other sources.
The MemoryService Role¶
The BaseMemoryService defines the interface for managing this searchable, long-term knowledge store. Its primary responsibilities are:

Ingesting Information (add_session_to_memory): Taking the contents of a (usually completed) Session and adding relevant information to the long-term knowledge store.
Searching Information (search_memory): Allowing an agent (typically via a Tool) to query the knowledge store and retrieve relevant snippets or context based on a search query.
Choosing the Right Memory Service¶
The ADK offers two distinct MemoryService implementations, each tailored to different use cases. Use the table below to decide which is the best fit for your agent.

Feature	InMemoryMemoryService	VertexAiMemoryBankService
Persistence	None (data is lost on restart)	Yes (Managed by Vertex AI)
Primary Use Case	Prototyping, local development, and simple testing.	Building meaningful, evolving memories from user conversations.
Memory Extraction	Stores full conversation	Extracts meaningful information from conversations and consolidates it with existing memories (powered by LLM)
Search Capability	Basic keyword matching.	Advanced semantic search.
Setup Complexity	None. It's the default.	Low. Requires an Agent Engine instance in Vertex AI.
Dependencies	None.	Google Cloud Project, Vertex AI API
When to use it	When you want to search across multiple sessions’ chat histories for prototyping.	When you want your agent to remember and learn from past interactions.
In-Memory Memory¶
The InMemoryMemoryService stores session information in the application's memory and performs basic keyword matching for searches. It requires no setup and is best for prototyping and simple testing scenarios where persistence isn't required.


Python
Go

import (
  "google.golang.org/adk/memory"
  "google.golang.org/adk/session"
)

// Services must be shared across runners to share state and memory.
sessionService := session.InMemoryService()
memoryService := memory.InMemoryService()

Example: Adding and Searching Memory

This example demonstrates the basic flow using the InMemoryMemoryService for simplicity.


Python
Go

import (
    "context"
    "fmt"
    "log"
    "strings"

    "google.golang.org/adk/agent"
    "google.golang.org/adk/agent/llmagent"
    "google.golang.org/adk/memory"
    "google.golang.org/adk/model/gemini"
    "google.golang.org/adk/runner"
    "google.golang.org/adk/session"
    "google.golang.org/adk/tool"
    "google.golang.org/adk/tool/functiontool"
    "google.golang.org/genai"
)

const (
    appName = "go_memory_example_app"
    userID  = "go_mem_user"
    modelID = "gemini-2.5-pro"
)

// Args defines the input structure for the memory search tool.
type Args struct {
    Query string `json:"query" jsonschema:"The query to search for in the memory."`
}

// Result defines the output structure for the memory search tool.
type Result struct {
    Results []string `json:"results"`
}


// memorySearchToolFunc is the implementation of the memory search tool.
// This function demonstrates accessing memory via tool.Context.
func memorySearchToolFunc(tctx tool.Context, args Args) (Result, error) {
    fmt.Printf("Tool: Searching memory for query: '%s'\n", args.Query)
    // The SearchMemory function is available on the context.
    searchResults, err := tctx.SearchMemory(context.Background(), args.Query)
    if err != nil {
        log.Printf("Error searching memory: %v", err)
        return Result{}, fmt.Errorf("failed memory search")
    }

    var results []string
    for _, res := range searchResults.Memories {
        if res.Content != nil {
            results = append(results, textParts(res.Content)...)
        }
    }
    return Result{Results: results}, nil
}

// Define a tool that can search memory.
var memorySearchTool = must(functiontool.New(
    functiontool.Config{
        Name:        "search_past_conversations",
        Description: "Searches past conversations for relevant information.",
    },
    memorySearchToolFunc,
))


// This example demonstrates how to use the MemoryService in the Go ADK.
// It covers two main scenarios:
// 1. Adding a completed session to memory and recalling it in a new session.
// 2. Searching memory from within a custom tool using the tool.Context.
func main() {
    ctx := context.Background()

    // --- Services ---
    // Services must be shared across runners to share state and memory.
    sessionService := session.InMemoryService()
    memoryService := memory.InMemoryService() // Use in-memory for this demo.

    // --- Scenario 1: Capture information in one session ---
    fmt.Println("--- Turn 1: Capturing Information ---")
    infoCaptureAgent := must(llmagent.New(llmagent.Config{
        Name:        "InfoCaptureAgent",
        Model:       must(gemini.NewModel(ctx, modelID, nil)),
        Instruction: "Acknowledge the user's statement.",
    }))

    runner1 := must(runner.New(runner.Config{
        AppName:        appName,
        Agent:          infoCaptureAgent,
        SessionService: sessionService,
        MemoryService:  memoryService, // Provide the memory service to the Runner
    }))

    session1ID := "session_info"
    must(sessionService.Create(ctx, &session.CreateRequest{AppName: appName, UserID: userID, SessionID: session1ID}))

    userInput1 := genai.NewContentFromText("My favorite project is Project Alpha.", "user")
    var finalResponseText string
    for event, err := range runner1.Run(ctx, userID, session1ID, userInput1, agent.RunConfig{}) {
        if err != nil {
            log.Printf("Agent 1 Error: %v", err)
            continue
        }
        if event.Content != nil && !event.LLMResponse.Partial {
            finalResponseText = strings.Join(textParts(event.LLMResponse.Content), "")
        }
    }
    fmt.Printf("Agent 1 Response: %s\n", finalResponseText)

    // Add the completed session to the Memory Service
    fmt.Println("\n--- Adding Session 1 to Memory ---")
    resp, err := sessionService.Get(ctx, &session.GetRequest{AppName: appName, UserID: userID, SessionID: session1ID})
    if err != nil {
        log.Fatalf("Failed to get completed session: %v", err)
    }
    if err := memoryService.AddSession(ctx, resp.Session); err != nil {
        log.Fatalf("Failed to add session to memory: %v", err)
    }
    fmt.Println("Session added to memory.")

    // --- Scenario 2: Recall the information in a new session using a tool ---
    fmt.Println("\n--- Turn 2: Recalling Information ---")

    memoryRecallAgent := must(llmagent.New(llmagent.Config{
        Name:        "MemoryRecallAgent",
        Model:       must(gemini.NewModel(ctx, modelID, nil)),
        Instruction: "Answer the user's question. Use the 'search_past_conversations' tool if the answer might be in past conversations.",
        Tools:       []tool.Tool{memorySearchTool}, // Give the agent the tool
    }))

    runner2 := must(runner.New(runner.Config{
        Agent:          memoryRecallAgent,
        AppName:        appName,
        SessionService: sessionService,
        MemoryService:  memoryService,
    }))

    session2ID := "session_recall"
    must(sessionService.Create(ctx, &session.CreateRequest{AppName: appName, UserID: userID, SessionID: session2ID}))
    userInput2 := genai.NewContentFromText("What is my favorite project?", "user")

    var finalResponseText2 string
    for event, err := range runner2.Run(ctx, userID, session2ID, userInput2, agent.RunConfig{}) {
        if err != nil {
            log.Printf("Agent 2 Error: %v", err)
            continue
        }
        if event.Content != nil && !event.LLMResponse.Partial {
            finalResponseText2 = strings.Join(textParts(event.LLMResponse.Content), "")
        }
    }
    fmt.Printf("Agent 2 Response: %s\n", finalResponseText2)
}

Searching Memory Within a Tool¶
You can also search memory from within a custom tool by using the tool.Context.


Go

// memorySearchToolFunc is the implementation of the memory search tool.
// This function demonstrates accessing memory via tool.Context.
func memorySearchToolFunc(tctx tool.Context, args Args) (Result, error) {
    fmt.Printf("Tool: Searching memory for query: '%s'\n", args.Query)
    // The SearchMemory function is available on the context.
    searchResults, err := tctx.SearchMemory(context.Background(), args.Query)
    if err != nil {
        log.Printf("Error searching memory: %v", err)
        return Result{}, fmt.Errorf("failed memory search")
    }

    var results []string
    for _, res := range searchResults.Memories {
        if res.Content != nil {
            results = append(results, textParts(res.Content)...)
        }
    }
    return Result{Results: results}, nil
}

// Define a tool that can search memory.
var memorySearchTool = must(functiontool.New(
    functiontool.Config{
        Name:        "search_past_conversations",
        Description: "Searches past conversations for relevant information.",
    },
    memorySearchToolFunc,
))

Vertex AI Memory Bank¶
The VertexAiMemoryBankService connects your agent to Vertex AI Memory Bank, a fully managed Google Cloud service that provides sophisticated, persistent memory capabilities for conversational agents.

How It Works¶
The service handles two key operations:

Generating Memories: At the end of a conversation, you can send the session's events to the Memory Bank, which intelligently processes and stores the information as "memories."
Retrieving Memories: Your agent code can issue a search query against the Memory Bank to retrieve relevant memories from past conversations.
Prerequisites¶
Before you can use this feature, you must have:

A Google Cloud Project: With the Vertex AI API enabled.
An Agent Engine: You need to create an Agent Engine in Vertex AI. You do not need to deploy your agent to Agent Engine Runtime to use Memory Bank. This will provide you with the Agent Engine ID required for configuration.
Authentication: Ensure your local environment is authenticated to access Google Cloud services. The simplest way is to run:

gcloud auth application-default login
Environment Variables: The service requires your Google Cloud Project ID and Location. Set them as environment variables:

export GOOGLE_CLOUD_PROJECT="your-gcp-project-id"
export GOOGLE_CLOUD_LOCATION="your-gcp-location"
Configuration¶
To connect your agent to the Memory Bank, you use the --memory_service_uri flag when starting the ADK server (adk web or adk api_server). The URI must be in the format agentengine://<agent_engine_id>.

bash

adk web path/to/your/agents_dir --memory_service_uri="agentengine://1234567890"
Or, you can configure your agent to use the Memory Bank by manually instantiating the VertexAiMemoryBankService and passing it to the Runner.


Python


from google.adk.memory import VertexAiMemoryBankService

agent_engine_id = agent_engine.api_resource.name.split("/")[-1]

memory_service = VertexAiMemoryBankService(
    project="PROJECT_ID",
    location="LOCATION",
    agent_engine_id=agent_engine_id
)

runner = adk.Runner(
    ...
    memory_service=memory_service
)
Using Memory in Your Agent¶
When a memory service is configured, your agent can use a tool or callback to retrieve memories. ADK includes two pre-built tools for retrieving memories:

PreloadMemory: Always retrieve memory at the beginning of each turn (similar to a callback).
LoadMemory: Retrieve memory when your agent decides it would be helpful.
Example:


Python


from google.adk.agents import Agent
from google.adk.tools.preload_memory_tool import PreloadMemoryTool

agent = Agent(
    model=MODEL_ID,
    name='weather_sentiment_agent',
    instruction="...",
    tools=[PreloadMemoryTool()]
)
To extract memories from your session, you need to call add_session_to_memory. For example, you can automate this via a callback:


Python


from google import adk

async def auto_save_session_to_memory_callback(callback_context):
    await callback_context._invocation_context.memory_service.add_session_to_memory(
        callback_context._invocation_context.session)

agent = Agent(
    model=MODEL,
    name="Generic_QA_Agent",
    instruction="Answer the user's questions",
    tools=[adk.tools.preload_memory_tool.PreloadMemoryTool()],
    after_agent_callback=auto_save_session_to_memory_callback,
)
Advanced Concepts¶
How Memory Works in Practice¶
The memory workflow internally involves these steps:

Session Interaction: A user interacts with an agent via a Session, managed by a SessionService. Events are added, and state might be updated.
Ingestion into Memory: At some point (often when a session is considered complete or has yielded significant information), your application calls memory_service.add_session_to_memory(session). This extracts relevant information from the session's events and adds it to the long-term knowledge store (in-memory dictionary or Agent Engine Memory Bank).
Later Query: In a different (or the same) session, the user might ask a question requiring past context (e.g., "What did we discuss about project X last week?").
Agent Uses Memory Tool: An agent equipped with a memory-retrieval tool (like the built-in load_memory tool) recognizes the need for past context. It calls the tool, providing a search query (e.g., "discussion project X last week").
Search Execution: The tool internally calls memory_service.search_memory(app_name, user_id, query).
Results Returned: The MemoryService searches its store (using keyword matching or semantic search) and returns relevant snippets as a SearchMemoryResponse containing a list of MemoryResult objects (each potentially holding events from a relevant past session).
Agent Uses Results: The tool returns these results to the agent, usually as part of the context or function response. The agent can then use this retrieved information to formulate its final answer to the user.
Can an agent have access to more than one memory service?¶
Through Standard Configuration: No. The framework (adk web, adk api_server) is designed to be configured with one single memory service at a time via the --memory_service_uri flag. This single service is then provided to the agent and accessed through the built-in self.search_memory() method. From a configuration standpoint, you can only choose one backend (InMemory, VertexAiMemoryBankService) for all agents served by that process.

Within Your Agent's Code: Yes, absolutely. There is nothing preventing you from manually importing and instantiating another memory service directly inside your agent's code. This allows you to access multiple memory sources within a single agent turn.

For example, your agent could use the framework-configured InMemoryMemoryService to recall conversational history, and also manually instantiate a VertexAiMemoryBankService to look up information in a technical manual.

Example: Using Two Memory Services¶
Here’s how you could implement that in your agent's code:


Python


from google.adk.agents import Agent
from google.adk.memory import InMemoryMemoryService, VertexAiMemoryBankService
from google.genai import types

class MultiMemoryAgent(Agent):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)

        self.memory_service = InMemoryMemoryService()
        # Manually instantiate a second memory service for document lookups
        self.vertexai_memorybank_service = VertexAiMemoryBankService(
            project="PROJECT_ID",
            location="LOCATION",
            agent_engine_id="AGENT_ENGINE_ID"
        )

    async def run(self, request: types.Content, **kwargs) -> types.Content:
        user_query = request.parts[0].text

        # 1. Search conversational history using the framework-provided memory
        #    (This would be InMemoryMemoryService if configured)
        conversation_context = await self.memory_service.search_memory(query=user_query)

        # 2. Search the document knowledge base using the manually created service
        document_context = await self.vertexai_memorybank_service.search_memory(query=user_query)

        # Combine the context from both sources to generate a better response
        prompt = "From our past conversations, I remember:\n"
        prompt += f"{conversation_context.memories}\n\n"
        prompt += "From the technical manuals, I found:\n"
        prompt += f"{document_context.memories}\n\n"
        prompt += f"Based on all this, here is my answer to '{user_query}':"

        return await self.llm.generate_content_async(prompt)