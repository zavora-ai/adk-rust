Custom Tools for ADK¶
Supported in ADKPython v0.1.0Go v0.1.0Java v0.1.0
In an ADK agent workflow, Tools are programming functions with structured input and output that can be called by an ADK Agent to perform actions. ADK Tools function similarly to how you use a Function Call with Gemini or other generative AI models. You can perform various actions and programming functions with an ADK Tool, such as:

Querying databases
Making API requests: getting weather data, booking systems
Searching the web
Executing code snippets
Retrieving information from documents (RAG)
Interacting with other software or services
ADK Tools list

Before building your own Tools for ADK, check out the ADK Tools list for pre-built tools you can use with ADK Agents.

What is a Tool?¶
In the context of ADK, a Tool represents a specific capability provided to an AI agent, enabling it to perform actions and interact with the world beyond its core text generation and reasoning abilities. What distinguishes capable agents from basic language models is often their effective use of tools.

Technically, a tool is typically a modular code component—like a Python/ Java function, a class method, or even another specialized agent—designed to execute a distinct, predefined task. These tasks often involve interacting with external systems or data.

Agent tool call

Key Characteristics¶
Action-Oriented: Tools perform specific actions for an agent, such as searching for information, calling an API, or performing calculations.

Extends Agent capabilities: They empower agents to access real-time information, affect external systems, and overcome the knowledge limitations inherent in their training data.

Execute predefined logic: Crucially, tools execute specific, developer-defined logic. They do not possess their own independent reasoning capabilities like the agent's core Large Language Model (LLM). The LLM reasons about which tool to use, when, and with what inputs, but the tool itself just executes its designated function.

How Agents Use Tools¶
Agents leverage tools dynamically through mechanisms often involving function calling. The process generally follows these steps:

Reasoning: The agent's LLM analyzes its system instruction, conversation history, and user request.
Selection: Based on the analysis, the LLM decides on which tool, if any, to execute, based on the tools available to the agent and the docstrings that describes each tool.
Invocation: The LLM generates the required arguments (inputs) for the selected tool and triggers its execution.
Observation: The agent receives the output (result) returned by the tool.
Finalization: The agent incorporates the tool's output into its ongoing reasoning process to formulate the next response, decide the subsequent step, or determine if the goal has been achieved.
Think of the tools as a specialized toolkit that the agent's intelligent core (the LLM) can access and utilize as needed to accomplish complex tasks.

Tool Types in ADK¶
ADK offers flexibility by supporting several types of tools:

Function Tools: Tools created by you, tailored to your specific application's needs.
Functions/Methods: Define standard synchronous functions or methods in your code (e.g., Python def).
Agents-as-Tools: Use another, potentially specialized, agent as a tool for a parent agent.
Long Running Function Tools: Support for tools that perform asynchronous operations or take significant time to complete.
Built-in Tools: Ready-to-use tools provided by the framework for common tasks. Examples: Google Search, Code Execution, Retrieval-Augmented Generation (RAG).
Third-Party Tools: Integrate tools seamlessly from popular external libraries.
Navigate to the respective documentation pages linked above for detailed information and examples for each tool type.

Referencing Tool in Agent’s Instructions¶
Within an agent's instructions, you can directly reference a tool by using its function name. If the tool's function name and docstring are sufficiently descriptive, your instructions can primarily focus on when the Large Language Model (LLM) should utilize the tool. This promotes clarity and helps the model understand the intended use of each tool.

It is crucial to clearly instruct the agent on how to handle different return values that a tool might produce. For example, if a tool returns an error message, your instructions should specify whether the agent should retry the operation, give up on the task, or request additional information from the user.

Furthermore, ADK supports the sequential use of tools, where the output of one tool can serve as the input for another. When implementing such workflows, it's important to describe the intended sequence of tool usage within the agent's instructions to guide the model through the necessary steps.

Example¶
The following example showcases how an agent can use tools by referencing their function names in its instructions. It also demonstrates how to guide the agent to handle different return values from tools, such as success or error messages, and how to orchestrate the sequential use of multiple tools to accomplish a task.


Python
Go
Java

// Copyright 2025 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

package main

import (
    "context"
    "fmt"
    "log"
    "strings"

    "google.golang.org/adk/agent"
    "google.golang.org/adk/agent/llmagent"
    "google.golang.org/adk/model/gemini"
    "google.golang.org/adk/runner"
    "google.golang.org/adk/session"
    "google.golang.org/adk/tool"
    "google.golang.org/adk/tool/functiontool"
    "google.golang.org/genai"
)

type getWeatherReportArgs struct {
    City string `json:"city" jsonschema:"The city for which to get the weather report."`
}

type getWeatherReportResult struct {
    Status string `json:"status"`
    Report string `json:"report,omitempty"`
}

func getWeatherReport(ctx tool.Context, args getWeatherReportArgs) (getWeatherReportResult, error) {
    if strings.ToLower(args.City) == "london" {
        return getWeatherReportResult{Status: "success", Report: "The current weather in London is cloudy with a temperature of 18 degrees Celsius and a chance of rain."}, nil
    }
    if strings.ToLower(args.City) == "paris" {
        return getWeatherReportResult{Status: "success", Report: "The weather in Paris is sunny with a temperature of 25 degrees Celsius."}, nil
    }
    return getWeatherReportResult{}, fmt.Errorf("weather information for '%s' is not available.", args.City)
}

type analyzeSentimentArgs struct {
    Text string `json:"text" jsonschema:"The text to analyze for sentiment."`
}

type analyzeSentimentResult struct {
    Sentiment  string  `json:"sentiment"`
    Confidence float64 `json:"confidence"`
}

func analyzeSentiment(ctx tool.Context, args analyzeSentimentArgs) (analyzeSentimentResult, error) {
    if strings.Contains(strings.ToLower(args.Text), "good") || strings.Contains(strings.ToLower(args.Text), "sunny") {
        return analyzeSentimentResult{Sentiment: "positive", Confidence: 0.8}, nil
    }
    if strings.Contains(strings.ToLower(args.Text), "rain") || strings.Contains(strings.ToLower(args.Text), "bad") {
        return analyzeSentimentResult{Sentiment: "negative", Confidence: 0.7}, nil
    }
    return analyzeSentimentResult{Sentiment: "neutral", Confidence: 0.6}, nil
}

func main() {
    ctx := context.Background()
    model, err := gemini.NewModel(ctx, "gemini-2.0-flash", &genai.ClientConfig{})
    if err != nil {
        log.Fatal(err)
    }

    weatherTool, err := functiontool.New(
        functiontool.Config{
            Name:        "get_weather_report",
            Description: "Retrieves the current weather report for a specified city.",
        },
        getWeatherReport,
    )
    if err != nil {
        log.Fatal(err)
    }

    sentimentTool, err := functiontool.New(
        functiontool.Config{
            Name:        "analyze_sentiment",
            Description: "Analyzes the sentiment of the given text.",
        },
        analyzeSentiment,
    )
    if err != nil {
        log.Fatal(err)
    }

    weatherSentimentAgent, err := llmagent.New(llmagent.Config{
        Name:        "weather_sentiment_agent",
        Model:       model,
        Instruction: "You are a helpful assistant that provides weather information and analyzes the sentiment of user feedback. **If the user asks about the weather in a specific city, use the 'get_weather_report' tool to retrieve the weather details.** **If the 'get_weather_report' tool returns a 'success' status, provide the weather report to the user.** **If the 'get_weather_report' tool returns an 'error' status, inform the user that the weather information for the specified city is not available and ask if they have another city in mind.** **After providing a weather report, if the user gives feedback on the weather (e.g., 'That's good' or 'I don't like rain'), use the 'analyze_sentiment' tool to understand their sentiment.** Then, briefly acknowledge their sentiment. You can handle these tasks sequentially if needed.",
        Tools:       []tool.Tool{weatherTool, sentimentTool},
    })
    if err != nil {
        log.Fatal(err)
    }

    sessionService := session.InMemoryService()
    runner, err := runner.New(runner.Config{
        AppName:        "weather_sentiment_agent",
        Agent:          weatherSentimentAgent,
        SessionService: sessionService,
    })
    if err != nil {
        log.Fatal(err)
    }

    session, err := sessionService.Create(ctx, &session.CreateRequest{
        AppName: "weather_sentiment_agent",
        UserID:  "user1234",
    })
    if err != nil {
        log.Fatal(err)
    }

    run(ctx, runner, session.Session.ID(), "weather in london?")
    run(ctx, runner, session.Session.ID(), "I don't like rain.")
}

func run(ctx context.Context, r *runner.Runner, sessionID string, prompt string) {
    fmt.Printf("\n> %s\n", prompt)
    events := r.Run(
        ctx,
        "user1234",
        sessionID,
        genai.NewContentFromText(prompt, genai.RoleUser),
        agent.RunConfig{
            StreamingMode: agent.StreamingModeNone,
        },
    )
    for event, err := range events {
        if err != nil {
            log.Fatalf("ERROR during agent execution: %v", err)
        }

        if event.Content.Parts[0].Text != "" {
            fmt.Printf("Agent Response: %s\n", event.Content.Parts[0].Text)
        }
    }
}

Tool Context¶
For more advanced scenarios, ADK allows you to access additional contextual information within your tool function by including the special parameter tool_context: ToolContext. By including this in the function signature, ADK will automatically provide an instance of the ToolContext class when your tool is called during agent execution.

The ToolContext provides access to several key pieces of information and control levers:

state: State: Read and modify the current session's state. Changes made here are tracked and persisted.

actions: EventActions: Influence the agent's subsequent actions after the tool runs (e.g., skip summarization, transfer to another agent).

function_call_id: str: The unique identifier assigned by the framework to this specific invocation of the tool. Useful for tracking and correlating with authentication responses. This can also be helpful when multiple tools are called within a single model response.

function_call_event_id: str: This attribute provides the unique identifier of the event that triggered the current tool call. This can be useful for tracking and logging purposes.

auth_response: Any: Contains the authentication response/credentials if an authentication flow was completed before this tool call.

Access to Services: Methods to interact with configured services like Artifacts and Memory.

Note that you shouldn't include the tool_context parameter in the tool function docstring. Since ToolContext is automatically injected by the ADK framework after the LLM decides to call the tool function, it is not relevant for the LLM's decision-making and including it can confuse the LLM.

State Management¶
The tool_context.state attribute provides direct read and write access to the state associated with the current session. It behaves like a dictionary but ensures that any modifications are tracked as deltas and persisted by the session service. This enables tools to maintain and share information across different interactions and agent steps.

Reading State: Use standard dictionary access (tool_context.state['my_key']) or the .get() method (tool_context.state.get('my_key', default_value)).

Writing State: Assign values directly (tool_context.state['new_key'] = 'new_value'). These changes are recorded in the state_delta of the resulting event.

State Prefixes: Remember the standard state prefixes:

app:*: Shared across all users of the application.

user:*: Specific to the current user across all their sessions.

(No prefix): Specific to the current session.

temp:*: Temporary, not persisted across invocations (useful for passing data within a single run call but generally less useful inside a tool context which operates between LLM calls).


Python
Go
Java

import (
    "fmt"

    "google.golang.org/adk/tool"
)

type updateUserPreferenceArgs struct {
    Preference string `json:"preference" jsonschema:"The name of the preference to set."`
    Value      string `json:"value" jsonschema:"The value to set for the preference."`
}

type updateUserPreferenceResult struct {
    UpdatedPreference string `json:"updated_preference"`
}

func updateUserPreference(ctx tool.Context, args updateUserPreferenceArgs) (*updateUserPreferenceResult, error) {
    userPrefsKey := "user:preferences"
    val, err := ctx.State().Get(userPrefsKey)
    if err != nil {
        val = make(map[string]any)
    }

    preferencesMap, ok := val.(map[string]any)
    if !ok {
        preferencesMap = make(map[string]any)
    }

    preferencesMap[args.Preference] = args.Value

    if err := ctx.State().Set(userPrefsKey, preferencesMap); err != nil {
        return nil, err
    }

    fmt.Printf("Tool: Updated user preference '%s' to '%s'\n", args.Preference, args.Value)
    return &updateUserPreferenceResult{
        UpdatedPreference: args.Preference,
    }, nil
}

Controlling Agent Flow¶
The tool_context.actions attribute (ToolContext.actions() in Java and tool.Context.Actions() in Go) holds an EventActions object. Modifying attributes on this object allows your tool to influence what the agent or framework does after the tool finishes execution.

skip_summarization: bool: (Default: False) If set to True, instructs the ADK to bypass the LLM call that typically summarizes the tool's output. This is useful if your tool's return value is already a user-ready message.

transfer_to_agent: str: Set this to the name of another agent. The framework will halt the current agent's execution and transfer control of the conversation to the specified agent. This allows tools to dynamically hand off tasks to more specialized agents.

escalate: bool: (Default: False) Setting this to True signals that the current agent cannot handle the request and should pass control up to its parent agent (if in a hierarchy). In a LoopAgent, setting escalate=True in a sub-agent's tool will terminate the loop.

Example¶

Python
Go
Java

// Copyright 2025 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

package main

import (
    "context"
    "fmt"
    "log"
    "strings"

    "google.golang.org/adk/agent"
    "google.golang.org/adk/agent/llmagent"
    "google.golang.org/adk/model/gemini"
    "google.golang.org/adk/runner"
    "google.golang.org/adk/session"
    "google.golang.org/adk/tool"
    "google.golang.org/adk/tool/functiontool"
    "google.golang.org/genai"
)

type checkAndTransferArgs struct {
    Query string `json:"query" jsonschema:"The user's query to check for urgency."`
}

type checkAndTransferResult struct {
    Status string `json:"status"`
}

func checkAndTransfer(ctx tool.Context, args checkAndTransferArgs) (checkAndTransferResult, error) {
    if strings.Contains(strings.ToLower(args.Query), "urgent") {
        fmt.Println("Tool: Detected urgency, transferring to the support agent.")
        ctx.Actions().TransferToAgent = "support_agent"
        return checkAndTransferResult{Status: "Transferring to the support agent..."}, nil
    }
    return checkAndTransferResult{Status: fmt.Sprintf("Processed query: '%s'. No further action needed.", args.Query)}, nil
}

func main() {
    ctx := context.Background()
    model, err := gemini.NewModel(ctx, "gemini-2.0-flash", &genai.ClientConfig{})
    if err != nil {
        log.Fatal(err)
    }

    supportAgent, err := llmagent.New(llmagent.Config{
        Name:        "support_agent",
        Model:       model,
        Instruction: "You are the dedicated support agent. Mentioned you are a support handler and please help the user with their urgent issue.",
    })
    if err != nil {
        log.Fatal(err)
    }

    checkAndTransferTool, err := functiontool.New(
        functiontool.Config{
            Name:        "check_and_transfer",
            Description: "Checks if the query requires escalation and transfers to another agent if needed.",
        },
        checkAndTransfer,
    )
    if err != nil {
        log.Fatal(err)
    }

    mainAgent, err := llmagent.New(llmagent.Config{
        Name:        "main_agent",
        Model:       model,
        Instruction: "You are the first point of contact for customer support of an analytics tool. Answer general queries. If the user indicates urgency, use the 'check_and_transfer' tool.",
        Tools:       []tool.Tool{checkAndTransferTool},
        SubAgents:   []agent.Agent{supportAgent},
    })
    if err != nil {
        log.Fatal(err)
    }

    sessionService := session.InMemoryService()
    runner, err := runner.New(runner.Config{
        AppName:        "customer_support_agent",
        Agent:          mainAgent,
        SessionService: sessionService,
    })
    if err != nil {
        log.Fatal(err)
    }

    session, err := sessionService.Create(ctx, &session.CreateRequest{
        AppName: "customer_support_agent",
        UserID:  "user1234",
    })
    if err != nil {
        log.Fatal(err)
    }

    run(ctx, runner, session.Session.ID(), "this is urgent, i cant login")
}

func run(ctx context.Context, r *runner.Runner, sessionID string, prompt string) {
    fmt.Printf("\n> %s\n", prompt)
    events := r.Run(
        ctx,
        "user1234",
        sessionID,
        genai.NewContentFromText(prompt, genai.RoleUser),
        agent.RunConfig{
            StreamingMode: agent.StreamingModeNone,
        },
    )
    for event, err := range events {
        if err != nil {
            log.Fatalf("ERROR during agent execution: %v", err)
        }

        if event.Content.Parts[0].Text != "" {
            fmt.Printf("Agent Response: %s\n", event.Content.Parts[0].Text)
        }
    }
}

Explanation¶
We define two agents: main_agent and support_agent. The main_agent is designed to be the initial point of contact.
The check_and_transfer tool, when called by main_agent, examines the user's query.
If the query contains the word "urgent", the tool accesses the tool_context, specifically tool_context.actions, and sets the transfer_to_agent attribute to support_agent.
This action signals to the framework to transfer the control of the conversation to the agent named support_agent.
When the main_agent processes the urgent query, the check_and_transfer tool triggers the transfer. The subsequent response would ideally come from the support_agent.
For a normal query without urgency, the tool simply processes it without triggering a transfer.
This example illustrates how a tool, through EventActions in its ToolContext, can dynamically influence the flow of the conversation by transferring control to another specialized agent.

Authentication¶
Supported in ADKPython v0.1.0
ToolContext provides mechanisms for tools interacting with authenticated APIs. If your tool needs to handle authentication, you might use the following:

auth_response: Contains credentials (e.g., a token) if authentication was already handled by the framework before your tool was called (common with RestApiTool and OpenAPI security schemes).

request_credential(auth_config: dict): Call this method if your tool determines authentication is needed but credentials aren't available. This signals the framework to start an authentication flow based on the provided auth_config.

get_auth_response(): Call this in a subsequent invocation (after request_credential was successfully handled) to retrieve the credentials the user provided.

For detailed explanations of authentication flows, configuration, and examples, please refer to the dedicated Tool Authentication documentation page.

Context-Aware Data Access Methods¶
These methods provide convenient ways for your tool to interact with persistent data associated with the session or user, managed by configured services.

list_artifacts() (or listArtifacts() in Java): Returns a list of filenames (or keys) for all artifacts currently stored for the session via the artifact_service. Artifacts are typically files (images, documents, etc.) uploaded by the user or generated by tools/agents.

load_artifact(filename: str): Retrieves a specific artifact by its filename from the artifact_service. You can optionally specify a version; if omitted, the latest version is returned. Returns a google.genai.types.Part object containing the artifact data and mime type, or None if not found.

save_artifact(filename: str, artifact: types.Part): Saves a new version of an artifact to the artifact_service. Returns the new version number (starting from 0).

search_memory(query: str): (Support in ADK Python and Go) Queries the user's long-term memory using the configured memory_service. This is useful for retrieving relevant information from past interactions or stored knowledge. The structure of the SearchMemoryResponse depends on the specific memory service implementation but typically contains relevant text snippets or conversation excerpts.

Example¶

Python
Go
Java

// Copyright 2025 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

package main

import (
    "fmt"

    "google.golang.org/adk/tool"
    "google.golang.org/genai"
)

type processDocumentArgs struct {
    DocumentName  string `json:"document_name" jsonschema:"The name of the document to be processed."`
    AnalysisQuery string `json:"analysis_query" jsonschema:"The query for the analysis."`
}

type processDocumentResult struct {
    Status           string `json:"status"`
    AnalysisArtifact string `json:"analysis_artifact,omitempty"`
    Version          int64  `json:"version,omitempty"`
    Message          string `json:"message,omitempty"`
}

func processDocument(ctx tool.Context, args processDocumentArgs) (*processDocumentResult, error) {
    fmt.Printf("Tool: Attempting to load artifact: %s\n", args.DocumentName)

    // List all artifacts
    listResponse, err := ctx.Artifacts().List(ctx)
    if err != nil {
        return nil, fmt.Errorf("failed to list artifacts")
    }

    fmt.Println("Tool: Available artifacts:")
    for _, file := range listResponse.FileNames {
        fmt.Printf(" - %s\n", file)
    }

    documentPart, err := ctx.Artifacts().Load(ctx, args.DocumentName)
    if err != nil {
        return nil, fmt.Errorf("document '%s' not found", args.DocumentName)
    }

    fmt.Printf("Tool: Loaded document '%s' of size %d bytes.\n", args.DocumentName, len(documentPart.Part.InlineData.Data))

    // 3. Search memory for related context
    fmt.Printf("Tool: Searching memory for context related to: '%s'\n", args.AnalysisQuery)
    memoryResp, err := ctx.SearchMemory(ctx, args.AnalysisQuery)
    if err != nil {
        fmt.Printf("Tool: Error searching memory: %v\n", err)
    }
    memoryResultCount := 0
    if memoryResp != nil {
        memoryResultCount = len(memoryResp.Memories)
    }
    fmt.Printf("Tool: Found %d memory results.\n", memoryResultCount)

    analysisResult := fmt.Sprintf("Analysis of '%s' regarding '%s' using memory context: [Placeholder Analysis Result]", args.DocumentName, args.AnalysisQuery)
    fmt.Println("Tool: Performed analysis.")

    analysisPart := genai.NewPartFromText(analysisResult)
    newArtifactName := fmt.Sprintf("analysis_%s", args.DocumentName)
    version, err := ctx.Artifacts().Save(ctx, newArtifactName, analysisPart)
    if err != nil {
        return nil, fmt.Errorf("failed to save artifact")
    }
    fmt.Printf("Tool: Saved analysis result as '%s' version %d.\n", newArtifactName, version.Version)

    return &processDocumentResult{
        Status:           "success",
        AnalysisArtifact: newArtifactName,
        Version:          version.Version,
    }, nil
}

By leveraging the ToolContext, developers can create more sophisticated and context-aware custom tools that seamlessly integrate with ADK's architecture and enhance the overall capabilities of their agents.

Defining Effective Tool Functions¶
When using a method or function as an ADK Tool, how you define it significantly impacts the agent's ability to use it correctly. The agent's Large Language Model (LLM) relies heavily on the function's name, parameters (arguments), type hints, and docstring / source code comments to understand its purpose and generate the correct call.

Here are key guidelines for defining effective tool functions:

Function Name:

Use descriptive, verb-noun based names that clearly indicate the action (e.g., get_weather, searchDocuments, schedule_meeting).
Avoid generic names like run, process, handle_data, or overly ambiguous names like doStuff. Even with a good description, a name like do_stuff might confuse the model about when to use the tool versus, for example, cancelFlight.
The LLM uses the function name as a primary identifier during tool selection.
Parameters (Arguments):

Your function can have any number of parameters.
Use clear and descriptive names (e.g., city instead of c, search_query instead of q).
Provide type hints in Python for all parameters (e.g., city: str, user_id: int, items: list[str]). This is essential for ADK to generate the correct schema for the LLM.
Ensure all parameter types are JSON serializable. All java primitives as well as standard Python types like str, int, float, bool, list, dict, and their combinations are generally safe. Avoid complex custom class instances as direct parameters unless they have a clear JSON representation.
Do not set default values for parameters. E.g., def my_func(param1: str = "default"). Default values are not reliably supported or used by the underlying models during function call generation. All necessary information should be derived by the LLM from the context or explicitly requested if missing.
self / cls Handled Automatically: Implicit parameters like self (for instance methods) or cls (for class methods) are automatically handled by ADK and excluded from the schema shown to the LLM. You only need to define type hints and descriptions for the logical parameters your tool requires the LLM to provide.
Return Type:

The function's return value must be a dictionary (dict) in Python or a Map in Java.
If your function returns a non-dictionary type (e.g., a string, number, list), the ADK framework will automatically wrap it into a dictionary/Map like {'result': your_original_return_value} before passing the result back to the model.
Design the dictionary/Map keys and values to be descriptive and easily understood by the LLM. Remember, the model reads this output to decide its next step.
Include meaningful keys. For example, instead of returning just an error code like 500, return {'status': 'error', 'error_message': 'Database connection failed'}.
It's a highly recommended practice to include a status key (e.g., 'success', 'error', 'pending', 'ambiguous') to clearly indicate the outcome of the tool execution for the model.
Docstring / Source Code Comments:

This is critical. The docstring is the primary source of descriptive information for the LLM.
Clearly state what the tool does. Be specific about its purpose and limitations.
Explain when the tool should be used. Provide context or example scenarios to guide the LLM's decision-making.
Describe each parameter clearly. Explain what information the LLM needs to provide for that argument.
Describe the structure and meaning of the expected dict return value, especially the different status values and associated data keys.
Do not describe the injected ToolContext parameter. Avoid mentioning the optional tool_context: ToolContext parameter within the docstring description since it is not a parameter the LLM needs to know about. ToolContext is injected by ADK, after the LLM decides to call it.
Example of a good definition:


Python
Go
Java

import (
    "fmt"

    "google.golang.org/adk/tool"
)

type lookupOrderStatusArgs struct {
    OrderID string `json:"order_id" jsonschema:"The ID of the order to look up."`
}

type order struct {
    State          string `json:"state"`
    TrackingNumber string `json:"tracking_number"`
}

type lookupOrderStatusResult struct {
    Status string `json:"status"`
    Order  order  `json:"order,omitempty"`
}

func lookupOrderStatus(ctx tool.Context, args lookupOrderStatusArgs) (*lookupOrderStatusResult, error) {
    // ... function implementation to fetch status ...
    statusDetails, ok := fetchStatusFromBackend(args.OrderID)
    if !ok {
        return nil, fmt.Errorf("order ID %s not found", args.OrderID)
    }
    return &lookupOrderStatusResult{
        Status: "success",
        Order: order{
            State:          statusDetails.State,
            TrackingNumber: statusDetails.Tracking,
        },
    }, nil
}

Simplicity and Focus:
Keep Tools Focused: Each tool should ideally perform one well-defined task.
Fewer Parameters are Better: Models generally handle tools with fewer, clearly defined parameters more reliably than those with many optional or complex ones.
Use Simple Data Types: Prefer basic types (str, int, bool, float, List[str], in Python, or int, byte, short, long, float, double, boolean and char in Java) over complex custom classes or deeply nested structures as parameters when possible.
Decompose Complex Tasks: Break down functions that perform multiple distinct logical steps into smaller, more focused tools. For instance, instead of a single update_user_profile(profile: ProfileObject) tool, consider separate tools like update_user_name(name: str), update_user_address(address: str), update_user_preferences(preferences: list[str]), etc. This makes it easier for the LLM to select and use the correct capability.
By adhering to these guidelines, you provide the LLM with the clarity and structure it needs to effectively utilize your custom function tools, leading to more capable and reliable agent behavior.