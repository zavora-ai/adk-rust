Context¶
Supported in ADKPython v0.1.0Go v0.1.0Java v0.1.0
In the Agent Development Kit (ADK), "context" refers to the crucial bundle of information available to your agent and its tools during specific operations. Think of it as the necessary background knowledge and resources needed to handle a current task or conversation turn effectively.

Agents often need more than just the latest user message to perform well. Context is essential because it enables:

Maintaining State: Remembering details across multiple steps in a conversation (e.g., user preferences, previous calculations, items in a shopping cart). This is primarily managed through session state.
Passing Data: Sharing information discovered or generated in one step (like an LLM call or a tool execution) with subsequent steps. Session state is key here too.
Accessing Services: Interacting with framework capabilities like:
Artifact Storage: Saving or loading files or data blobs (like PDFs, images, configuration files) associated with the session.
Memory: Searching for relevant information from past interactions or external knowledge sources connected to the user.
Authentication: Requesting and retrieving credentials needed by tools to access external APIs securely.
Identity and Tracking: Knowing which agent is currently running (agent.name) and uniquely identifying the current request-response cycle (invocation_id) for logging and debugging.
Tool-Specific Actions: Enabling specialized operations within tools, such as requesting authentication or searching memory, which require access to the current interaction's details.
The central piece holding all this information together for a single, complete user-request-to-final-response cycle (an invocation) is the InvocationContext. However, you typically won't create or manage this object directly. The ADK framework creates it when an invocation starts (e.g., via runner.run_async) and passes the relevant contextual information implicitly to your agent code, callbacks, and tools.


Python
Go
Java

/* Conceptual Pseudocode: How the framework provides context (Internal Logic) */
sessionService := session.InMemoryService()

r, err := runner.New(runner.Config{
    AppName:        appName,
    Agent:          myAgent,
    SessionService: sessionService,
})
if err != nil {
    log.Fatalf("Failed to create runner: %v", err)
}

s, err := sessionService.Create(ctx, &session.CreateRequest{
    AppName: appName,
    UserID:  userID,
})
if err != nil {
    log.Fatalf("FATAL: Failed to create session: %v", err)
}

scanner := bufio.NewScanner(os.Stdin)
for {
    fmt.Print("\nYou > ")
    if !scanner.Scan() {
        break
    }
    userInput := scanner.Text()
    if strings.EqualFold(userInput, "quit") {
        break
    }
    userMsg := genai.NewContentFromText(userInput, genai.RoleUser)
    events := r.Run(ctx, s.Session.UserID(), s.Session.ID(), userMsg, agent.RunConfig{
        StreamingMode: agent.StreamingModeNone,
    })
    fmt.Print("\nAgent > ")
    for event, err := range events {
        if err != nil {
            log.Printf("ERROR during agent execution: %v", err)
            break
        }
        fmt.Print(event.Content.Parts[0].Text)
    }
}

The Different types of Context¶
While InvocationContext acts as the comprehensive internal container, ADK provides specialized context objects tailored to specific situations. This ensures you have the right tools and permissions for the task at hand without needing to handle the full complexity of the internal context everywhere. Here are the different "flavors" you'll encounter:

InvocationContext

Where Used: Received as the ctx argument directly within an agent's core implementation methods (_run_async_impl, _run_live_impl).
Purpose: Provides access to the entire state of the current invocation. This is the most comprehensive context object.
Key Contents: Direct access to session (including state and events), the current agent instance, invocation_id, initial user_content, references to configured services (artifact_service, memory_service, session_service), and fields related to live/streaming modes.
Use Case: Primarily used when the agent's core logic needs direct access to the overall session or services, though often state and artifact interactions are delegated to callbacks/tools which use their own contexts. Also used to control the invocation itself (e.g., setting ctx.end_invocation = True).

Python
Go
Java

import (
    "google.golang.org/adk/agent"
    "google.golang.org/adk/session"
)

// Pseudocode: Agent implementation receiving InvocationContext
type MyAgent struct {
}

func (a *MyAgent) Run(ctx agent.InvocationContext) iter.Seq2[*session.Event, error] {
    return func(yield func(*session.Event, error) bool) {
        // Direct access example
        agentName := ctx.Agent().Name()
        sessionID := ctx.Session().ID()
        fmt.Printf("Agent %s running in session %s for invocation %s\n", agentName, sessionID, ctx.InvocationID())
        // ... agent logic using ctx ...
        yield(&session.Event{Author: agentName}, nil)
    }
}

ReadonlyContext

Where Used: Provided in scenarios where only read access to basic information is needed and mutation is disallowed (e.g., InstructionProvider functions). It's also the base class for other contexts.
Purpose: Offers a safe, read-only view of fundamental contextual details.
Key Contents: invocation_id, agent_name, and a read-only view of the current state.

Python
Go
Java

import "google.golang.org/adk/agent"

// Pseudocode: Instruction provider receiving ReadonlyContext
func myInstructionProvider(ctx agent.ReadonlyContext) (string, error) {
    // Read-only access example
    userTier, err := ctx.ReadonlyState().Get("user_tier")
    if err != nil {
        userTier = "standard" // Default value
    }
    // ctx.ReadonlyState() has no Set method since State() is read-only.
    return fmt.Sprintf("Process the request for a %v user.", userTier), nil
}

CallbackContext

Where Used: Passed as callback_context to agent lifecycle callbacks (before_agent_callback, after_agent_callback) and model interaction callbacks (before_model_callback, after_model_callback).
Purpose: Facilitates inspecting and modifying state, interacting with artifacts, and accessing invocation details specifically within callbacks.
Key Capabilities (Adds to ReadonlyContext):
Mutable state Property: Allows reading and writing to session state. Changes made here (callback_context.state['key'] = value) are tracked and associated with the event generated by the framework after the callback.
Artifact Methods: load_artifact(filename) and save_artifact(filename, part) methods for interacting with the configured artifact_service.
Direct user_content access.

Python
Go
Java

import (
    "google.golang.org/adk/agent"
    "google.golang.org/adk/model"
)

// Pseudocode: Callback receiving CallbackContext
func myBeforeModelCb(ctx agent.CallbackContext, req *model.LLMRequest) (*model.LLMResponse, error) {
    // Read/Write state example
    callCount, err := ctx.State().Get("model_calls")
    if err != nil {
        callCount = 0 // Default value
    }
    newCount := callCount.(int) + 1
    if err := ctx.State().Set("model_calls", newCount); err != nil {
        return nil, err
    }

    // Optionally load an artifact
    // configPart, err := ctx.Artifacts().Load("model_config.json")
    fmt.Printf("Preparing model call #%d for invocation %s\n", newCount, ctx.InvocationID())
    return nil, nil // Allow model call to proceed
}

ToolContext

Where Used: Passed as tool_context to the functions backing FunctionTools and to tool execution callbacks (before_tool_callback, after_tool_callback).
Purpose: Provides everything CallbackContext does, plus specialized methods essential for tool execution, like handling authentication, searching memory, and listing artifacts.
Key Capabilities (Adds to CallbackContext):
Authentication Methods: request_credential(auth_config) to trigger an auth flow, and get_auth_response(auth_config) to retrieve credentials provided by the user/system.
Artifact Listing: list_artifacts() to discover available artifacts in the session.
Memory Search: search_memory(query) to query the configured memory_service.
function_call_id Property: Identifies the specific function call from the LLM that triggered this tool execution, crucial for linking authentication requests or responses back correctly.
actions Property: Direct access to the EventActions object for this step, allowing the tool to signal state changes, auth requests, etc.

Python
Go
Java

import "google.golang.org/adk/tool"

// Pseudocode: Tool function receiving ToolContext
type searchExternalAPIArgs struct {
    Query string `json:"query" jsonschema:"The query to search for."`
}

func searchExternalAPI(tc tool.Context, input searchExternalAPIArgs) (string, error) {
    apiKey, err := tc.State().Get("api_key")
    if err != nil || apiKey == "" {
        // In a real scenario, you would define and request credentials here.
        // This is a conceptual placeholder.
        return "", fmt.Errorf("auth required")
    }

    // Use the API key...
    fmt.Printf("Tool executing for query '%s' using API key. Invocation: %s\n", input.Query, tc.InvocationID())

    // Optionally search memory or list artifacts
    // relevantDocs, _ := tc.SearchMemory(tc, "info related to %s", input.Query))
    // availableFiles, _ := tc.Artifacts().List()

    return fmt.Sprintf("Data for %s fetched.", input.Query), nil
}

Understanding these different context objects and when to use them is key to effectively managing state, accessing services, and controlling the flow of your ADK application. The next section will detail common tasks you can perform using these contexts.

Common Tasks Using Context¶
Now that you understand the different context objects, let's focus on how to use them for common tasks when building your agents and tools.

Accessing Information¶
You'll frequently need to read information stored within the context.

Reading Session State: Access data saved in previous steps or user/app-level settings. Use dictionary-like access on the state property.


Python
Go
Java

import (
    "google.golang.org/adk/agent"
    "google.golang.org/adk/session"
    "google.golang.org/adk/tool"
    "google.golang.org/genai"
)

// Pseudocode: In a Tool function
type toolArgs struct {
    // Define tool-specific arguments here
}

type toolResults struct {
    // Define tool-specific results here
}

// Example tool function demonstrating state access
func myTool(tc tool.Context, input toolArgs) (toolResults, error) {
    userPref, err := tc.State().Get("user_display_preference")
    if err != nil {
        userPref = "default_mode"
    }
    apiEndpoint, _ := tc.State().Get("app:api_endpoint") // Read app-level state

    if userPref == "dark_mode" {
        // ... apply dark mode logic ...
    }
    fmt.Printf("Using API endpoint: %v\n", apiEndpoint)
    // ... rest of tool logic ...
    return toolResults{}, nil
}


// Pseudocode: In a Callback function
func myCallback(ctx agent.CallbackContext) (*genai.Content, error) {
    lastToolResult, err := ctx.State().Get("temp:last_api_result") // Read temporary state
    if err == nil {
        fmt.Printf("Found temporary result from last tool: %v\n", lastToolResult)
    } else {
        fmt.Println("No temporary result found.")
    }
    // ... callback logic ...
    return nil, nil
}

Getting Current Identifiers: Useful for logging or custom logic based on the current operation.


Python
Go
Java

import "google.golang.org/adk/tool"

// Pseudocode: In any context (ToolContext shown)
type logToolUsageArgs struct{}
type logToolUsageResult struct {
    Status string `json:"status"`
}

func logToolUsage(tc tool.Context, args logToolUsageArgs) (logToolUsageResult, error) {
    agentName := tc.AgentName()
    invID := tc.InvocationID()
    funcCallID := tc.FunctionCallID()

    fmt.Printf("Log: Invocation=%s, Agent=%s, FunctionCallID=%s - Tool Executed.\n", invID, agentName, funcCallID)
    return logToolUsageResult{Status: "Logged successfully"}, nil
}

Accessing the Initial User Input: Refer back to the message that started the current invocation.


Python
Go
Java

import (
    "google.golang.org/adk/agent"
    "google.golang.org/genai"
)

// Pseudocode: In a Callback
func logInitialUserInput(ctx agent.CallbackContext) (*genai.Content, error) {
    userContent := ctx.UserContent()
    if userContent != nil && len(userContent.Parts) > 0 {
        if text := userContent.Parts[0].Text; text != "" {
            fmt.Printf("User's initial input for this turn: '%s'\n", text)
        }
    }
    return nil, nil // No modification
}

Managing State¶
State is crucial for memory and data flow. When you modify state using CallbackContext or ToolContext, the changes are automatically tracked and persisted by the framework.

How it Works: Writing to callback_context.state['my_key'] = my_value or tool_context.state['my_key'] = my_value adds this change to the EventActions.state_delta associated with the current step's event. The SessionService then applies these deltas when persisting the event.

Passing Data Between Tools


Python
Go
Java

import "google.golang.org/adk/tool"

// Pseudocode: Tool 1 - Fetches user ID
type GetUserProfileArgs struct {
}

func getUserProfile(tc tool.Context, input GetUserProfileArgs) (string, error) {
    // A random user ID for demonstration purposes
    userID := "random_user_456"

    // Save the ID to state for the next tool
    if err := tc.State().Set("temp:current_user_id", userID); err != nil {
        return "", fmt.Errorf("failed to set user ID in state: %w", err)
    }
    return "ID generated", nil
}


// Pseudocode: Tool 2 - Uses user ID from state
type GetUserOrdersArgs struct {
}

type getUserOrdersResult struct {
    Orders []string `json:"orders"`
}

func getUserOrders(tc tool.Context, input GetUserOrdersArgs) (*getUserOrdersResult, error) {
    userID, err := tc.State().Get("temp:current_user_id")
    if err != nil {
        return &getUserOrdersResult{}, fmt.Errorf("user ID not found in state")
    }

    fmt.Printf("Fetching orders for user ID: %v\n", userID)
    // ... logic to fetch orders using user_id ...
    return &getUserOrdersResult{Orders: []string{"order123", "order456"}}, nil
}

Updating User Preferences:


Python
Go
Java

import "google.golang.org/adk/tool"

// Pseudocode: Tool or Callback identifies a preference
type setUserPreferenceArgs struct {
    Preference string `json:"preference" jsonschema:"The name of the preference to set."`
    Value      string `json:"value" jsonschema:"The value to set for the preference."`
}

type setUserPreferenceResult struct {
    Status string `json:"status"`
}

func setUserPreference(tc tool.Context, args setUserPreferenceArgs) (setUserPreferenceResult, error) {
    // Use 'user:' prefix for user-level state (if using a persistent SessionService)
    stateKey := fmt.Sprintf("user:%s", args.Preference)
    if err := tc.State().Set(stateKey, args.Value); err != nil {
        return setUserPreferenceResult{}, fmt.Errorf("failed to set preference in state: %w", err)
    }
    fmt.Printf("Set user preference '%s' to '%s'\n", args.Preference, args.Value)
    return setUserPreferenceResult{Status: "Preference updated"}, nil
}

State Prefixes: While basic state is session-specific, prefixes like app: and user: can be used with persistent SessionService implementations (like DatabaseSessionService or VertexAiSessionService) to indicate broader scope (app-wide or user-wide across sessions). temp: can denote data only relevant within the current invocation.

Working with Artifacts¶
Use artifacts to handle files or large data blobs associated with the session. Common use case: processing uploaded documents.

Document Summarizer Example Flow:

Ingest Reference (e.g., in a Setup Tool or Callback): Save the path or URI of the document, not the entire content, as an artifact.


Python
Go
Java

import (
    "google.golang.org/adk/tool"
    "google.golang.org/genai"
)

// Adapt the saveDocumentReference callback into a tool for this example.
type saveDocRefArgs struct {
    FilePath string `json:"file_path" jsonschema:"The path to the file to save."`
}

type saveDocRefResult struct {
    Status string `json:"status"`
}

func saveDocRef(tc tool.Context, args saveDocRefArgs) (saveDocRefResult, error) {
    artifactPart := genai.NewPartFromText(args.FilePath)
    _, err := tc.Artifacts().Save(tc, "document_to_summarize.txt", artifactPart)
    if err != nil {
        return saveDocRefResult{}, err
    }
    fmt.Printf("Saved document reference '%s' as artifact\n", args.FilePath)
    if err := tc.State().Set("temp:doc_artifact_name", "document_to_summarize.txt"); err != nil {
        return saveDocRefResult{}, fmt.Errorf("failed to set artifact name in state")
    }
    return saveDocRefResult{"Reference saved"}, nil
}

Summarizer Tool: Load the artifact to get the path/URI, read the actual document content using appropriate libraries, summarize, and return the result.


Python
Go
Java

import "google.golang.org/adk/tool"

// Pseudocode: In the Summarizer tool function
type summarizeDocumentArgs struct{}

type summarizeDocumentResult struct {
    Summary string `json:"summary"`
}

func summarizeDocumentTool(tc tool.Context, input summarizeDocumentArgs) (summarizeDocumentResult, error) {
    artifactName, err := tc.State().Get("temp:doc_artifact_name")
    if err != nil {
        return summarizeDocumentResult{}, fmt.Errorf("No document artifact name found in state")
    }

    // 1. Load the artifact part containing the path/URI
    artifactPart, err := tc.Artifacts().Load(tc, artifactName.(string))
    if err != nil {
        return summarizeDocumentResult{}, err
    }

    if artifactPart.Part.Text == "" {
        return summarizeDocumentResult{}, fmt.Errorf("Could not load artifact or artifact has no text path.")
    }
    filePath := artifactPart.Part.Text
    fmt.Printf("Loaded document reference: %s\n", filePath)

    // 2. Read the actual document content (outside ADK context)
    // In a real implementation, you would use a GCS client or local file reader.
    documentContent := "This is the fake content of the document at " + filePath
    _ = documentContent // Avoid unused variable error.

    // 3. Summarize the content
    summary := "Summary of content from " + filePath // Placeholder

    return summarizeDocumentResult{Summary: summary}, nil
}

Listing Artifacts: Discover what files are available.


Python
Go
Java

import "google.golang.org/adk/tool"

// Pseudocode: In a tool function
type checkAvailableDocsArgs struct{}

type checkAvailableDocsResult struct {
    AvailableDocs []string `json:"available_docs"`
}

func checkAvailableDocs(tc tool.Context, args checkAvailableDocsArgs) (checkAvailableDocsResult, error) {
    artifactKeys, err := tc.Artifacts().List(tc)
    if err != nil {
        return checkAvailableDocsResult{}, err
    }
    fmt.Printf("Available artifacts: %v\n", artifactKeys)
    return checkAvailableDocsResult{AvailableDocs: artifactKeys.FileNames}, nil
}

Handling Tool Authentication¶
Supported in ADKPython v0.1.0
Securely manage API keys or other credentials needed by tools.


# Pseudocode: Tool requiring auth
from google.adk.tools import ToolContext
from google.adk.auth import AuthConfig # Assume appropriate AuthConfig is defined

# Define your required auth configuration (e.g., OAuth, API Key)
MY_API_AUTH_CONFIG = AuthConfig(...)
AUTH_STATE_KEY = "user:my_api_credential" # Key to store retrieved credential

def call_secure_api(tool_context: ToolContext, request_data: str) -> dict:
    # 1. Check if credential already exists in state
    credential = tool_context.state.get(AUTH_STATE_KEY)

    if not credential:
        # 2. If not, request it
        print("Credential not found, requesting...")
        try:
            tool_context.request_credential(MY_API_AUTH_CONFIG)
            # The framework handles yielding the event. The tool execution stops here for this turn.
            return {"status": "Authentication required. Please provide credentials."}
        except ValueError as e:
            return {"error": f"Auth error: {e}"} # e.g., function_call_id missing
        except Exception as e:
            return {"error": f"Failed to request credential: {e}"}

    # 3. If credential exists (might be from a previous turn after request)
    #    or if this is a subsequent call after auth flow completed externally
    try:
        # Optionally, re-validate/retrieve if needed, or use directly
        # This might retrieve the credential if the external flow just completed
        auth_credential_obj = tool_context.get_auth_response(MY_API_AUTH_CONFIG)
        api_key = auth_credential_obj.api_key # Or access_token, etc.

        # Store it back in state for future calls within the session
        tool_context.state[AUTH_STATE_KEY] = auth_credential_obj.model_dump() # Persist retrieved credential

        print(f"Using retrieved credential to call API with data: {request_data}")
        # ... Make the actual API call using api_key ...
        api_result = f"API result for {request_data}"

        return {"result": api_result}
    except Exception as e:
        # Handle errors retrieving/using the credential
        print(f"Error using credential: {e}")
        # Maybe clear the state key if credential is invalid?
        # tool_context.state[AUTH_STATE_KEY] = None
        return {"error": "Failed to use credential"}
Remember: request_credential pauses the tool and signals the need for authentication. The user/system provides credentials, and on a subsequent call, get_auth_response (or checking state again) allows the tool to proceed. The tool_context.function_call_id is used implicitly by the framework to link the request and response.
Leveraging Memory¶
Supported in ADKPython v0.1.0
Access relevant information from the past or external sources.


# Pseudocode: Tool using memory search
from google.adk.tools import ToolContext

def find_related_info(tool_context: ToolContext, topic: str) -> dict:
    try:
        search_results = tool_context.search_memory(f"Information about {topic}")
        if search_results.results:
            print(f"Found {len(search_results.results)} memory results for '{topic}'")
            # Process search_results.results (which are SearchMemoryResponseEntry)
            top_result_text = search_results.results[0].text
            return {"memory_snippet": top_result_text}
        else:
            return {"message": "No relevant memories found."}
    except ValueError as e:
        return {"error": f"Memory service error: {e}"} # e.g., Service not configured
    except Exception as e:
        return {"error": f"Unexpected error searching memory: {e}"}
Advanced: Direct InvocationContext Usage¶
Supported in ADKPython v0.1.0
While most interactions happen via CallbackContext or ToolContext, sometimes the agent's core logic (_run_async_impl/_run_live_impl) needs direct access.


# Pseudocode: Inside agent's _run_async_impl
from google.adk.agents import BaseAgent
from google.adk.agents.invocation_context import InvocationContext
from google.adk.events import Event
from typing import AsyncGenerator

class MyControllingAgent(BaseAgent):
    async def _run_async_impl(self, ctx: InvocationContext) -> AsyncGenerator[Event, None]:
        # Example: Check if a specific service is available
        if not ctx.memory_service:
            print("Memory service is not available for this invocation.")
            # Potentially change agent behavior

        # Example: Early termination based on some condition
        if ctx.session.state.get("critical_error_flag"):
            print("Critical error detected, ending invocation.")
            ctx.end_invocation = True # Signal framework to stop processing
            yield Event(author=self.name, invocation_id=ctx.invocation_id, content="Stopping due to critical error.")
            return # Stop this agent's execution

        # ... Normal agent processing ...
        yield # ... event ...
Setting ctx.end_invocation = True is a way to gracefully stop the entire request-response cycle from within the agent or its callbacks/tools (via their respective context objects which also have access to modify the underlying InvocationContext's flag).

Key Takeaways & Best Practices¶
Use the Right Context: Always use the most specific context object provided (ToolContext in tools/tool-callbacks, CallbackContext in agent/model-callbacks, ReadonlyContext where applicable). Use the full InvocationContext (ctx) directly in _run_async_impl / _run_live_impl only when necessary.
State for Data Flow: context.state is the primary way to share data, remember preferences, and manage conversational memory within an invocation. Use prefixes (app:, user:, temp:) thoughtfully when using persistent storage.
Artifacts for Files: Use context.save_artifact and context.load_artifact for managing file references (like paths or URIs) or larger data blobs. Store references, load content on demand.
Tracked Changes: Modifications to state or artifacts made via context methods are automatically linked to the current step's EventActions and handled by the SessionService.
Start Simple: Focus on state and basic artifact usage first. Explore authentication, memory, and advanced InvocationContext fields (like those for live streaming) as your needs become more complex.
By understanding and effectively using these context objects, you can build more sophisticated, stateful, and capable agents with ADK.