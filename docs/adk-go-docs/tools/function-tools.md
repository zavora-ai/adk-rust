Function tools¶
Supported in ADKPython v0.1.0Go v0.1.0Java v0.1.0
When pre-built ADK tools don't meet your requirements, you can create custom function tools. Building function tools allows you to create tailored functionality, such as connecting to proprietary databases or implementing unique algorithms. For example, a function tool, myfinancetool, might be a function that calculates a specific financial metric. ADK also supports long running functions, so if that calculation takes a while, the agent can continue working on other tasks.

ADK offers several ways to create functions tools, each suited to different levels of complexity and control:

Function Tools
Long Running Function Tools
Agents-as-a-Tool
Function Tools¶
Transforming a Python function into a tool is a straightforward way to integrate custom logic into your agents. When you assign a function to an agent’s tools list, the framework automatically wraps it as a FunctionTool.

How it Works¶
The ADK framework automatically inspects your Python function's signature—including its name, docstring, parameters, type hints, and default values—to generate a schema. This schema is what the LLM uses to understand the tool's purpose, when to use it, and what arguments it requires.

Defining Function Signatures¶
A well-defined function signature is crucial for the LLM to use your tool correctly.

Parameters¶
Required Parameters¶

Python
Go
In Go, you use struct tags to control the JSON schema. The two primary tags are json and jsonschema.

A parameter is considered required if its struct field does not have the omitempty or omitzero option in its json tag.

The jsonschema tag is used to provide the argument's description. This is crucial for the LLM to understand what the argument is for.

Example: Required Parameters

// GetWeatherParams defines the arguments for the getWeather tool.
type GetWeatherParams struct {
    // This field is REQUIRED (no "omitempty").
    // The jsonschema tag provides the description.
    Location string `json:"location" jsonschema:"The city and state, e.g., San Francisco, CA"`

    // This field is also REQUIRED.
    Unit     string `json:"unit" jsonschema:"The temperature unit, either 'celsius' or 'fahrenheit'"`
}
In this example, both location and unit are mandatory.


Optional Parameters¶

Python
Go
A parameter is considered optional if its struct field has the omitempty or omitzero option in its json tag.

Example: Optional Parameters

// GetWeatherParams defines the arguments for the getWeather tool.
type GetWeatherParams struct {
    // Location is required.
    Location string `json:"location" jsonschema:"The city and state, e.g., San Francisco, CA"`

    // Unit is optional.
    Unit string `json:"unit,omitempty" jsonschema:"The temperature unit, either 'celsius' or 'fahrenheit'"`

    // Days is optional.
    Days int `json:"days,omitzero" jsonschema:"The number of forecast days to return (defaults to 1)"`
}
Here, unit and days are optional. The LLM can choose to provide them, but they are not required.


Optional Parameters with typing.Optional¶
You can also mark a parameter as optional using typing.Optional[SomeType] or the | None syntax (Python 3.10+). This signals that the parameter can be None. When combined with a default value of None, it behaves as a standard optional parameter.

Example: typing.Optional

Python

from typing import Optional

def create_user_profile(username: str, bio: Optional[str] = None):
    """
    Creates a new user profile.

    Args:
        username (str): The user's unique username.
        bio (str, optional): A short biography for the user. Defaults to None.
    """
    # ... function logic ...
    if bio:
        return {"status": "success", "message": f"Profile for {username} created with a bio."}
    return {"status": "success", "message": f"Profile for {username} created."}

Variadic Parameters (*args and **kwargs)¶
While you can include *args (variable positional arguments) and **kwargs (variable keyword arguments) in your function signature for other purposes, they are ignored by the ADK framework when generating the tool schema for the LLM. The LLM will not be aware of them and cannot pass arguments to them. It's best to rely on explicitly defined parameters for all data you expect from the LLM.

Return Type¶
The preferred return type for a Function Tool is a dictionary in Python or Map in Java. This allows you to structure the response with key-value pairs, providing context and clarity to the LLM. If your function returns a type other than a dictionary, the framework automatically wraps it into a dictionary with a single key named "result".

Strive to make your return values as descriptive as possible. For example, instead of returning a numeric error code, return a dictionary with an "error_message" key containing a human-readable explanation. Remember that the LLM, not a piece of code, needs to understand the result. As a best practice, include a "status" key in your return dictionary to indicate the overall outcome (e.g., "success", "error", "pending"), providing the LLM with a clear signal about the operation's state.

Docstrings¶
The docstring of your function serves as the tool's description and is sent to the LLM. Therefore, a well-written and comprehensive docstring is crucial for the LLM to understand how to use the tool effectively. Clearly explain the purpose of the function, the meaning of its parameters, and the expected return values.

Passing Data Between Tools¶
When an agent calls multiple tools in a sequence, you might need to pass data from one tool to another. The recommended way to do this is by using the temp: prefix in the session state.

A tool can write data to a temp: variable, and a subsequent tool can read it. This data is only available for the current invocation and is discarded afterwards.

Shared Invocation Context

All tool calls within a single agent turn share the same InvocationContext. This means they also share the same temporary (temp:) state, which is how data can be passed between them.

Example¶
Example
Best Practices¶
While you have considerable flexibility in defining your function, remember that simplicity enhances usability for the LLM. Consider these guidelines:

Fewer Parameters are Better: Minimize the number of parameters to reduce complexity.
Simple Data Types: Favor primitive data types like str and int over custom classes whenever possible.
Meaningful Names: The function's name and parameter names significantly influence how the LLM interprets and utilizes the tool. Choose names that clearly reflect the function's purpose and the meaning of its inputs. Avoid generic names like do_stuff() or beAgent().
Build for Parallel Execution: Improve function calling performance when multiple tools are run by building for asynchronous operation. For information on enabling parallel execution for tools, see Increase tool performance with parallel execution.
Long Running Function Tools¶
This tool is designed to help you start and manage tasks that are handled outside the operation of your agent workflow, and require a significant amount of processing time, without blocking the agent's execution. This tool is a subclass of FunctionTool.

When using a LongRunningFunctionTool, your function can initiate the long-running operation and optionally return an initial result, such as a long-running operation id. Once a long running function tool is invoked the agent runner pauses the agent run and lets the agent client to decide whether to continue or wait until the long-running operation finishes. The agent client can query the progress of the long-running operation and send back an intermediate or final response. The agent can then continue with other tasks. An example is the human-in-the-loop scenario where the agent needs human approval before proceeding with a task.

Warning: Execution handling

Long Running Function Tools are designed to help you start and manage long running tasks as part of your agent workflow, but not perform the actual, long task. For tasks that require significant time to complete, you should implement a separate server to do the task.

Tip: Parallel execution

Depending on the type of tool you are building, designing for asychronous operation may be a better solution than creating a long running tool. For more information, see Increase tool performance with parallel execution.

How it Works¶
In Python, you wrap a function with LongRunningFunctionTool. In Java, you pass a Method name to LongRunningFunctionTool.create().

Initiation: When the LLM calls the tool, your function starts the long-running operation.

Initial Updates: Your function should optionally return an initial result (e.g. the long-running operaiton id). The ADK framework takes the result and sends it back to the LLM packaged within a FunctionResponse. This allows the LLM to inform the user (e.g., status, percentage complete, messages). And then the agent run is ended / paused.

Continue or Wait: After each agent run is completed. Agent client can query the progress of the long-running operation and decide whether to continue the agent run with an intermediate response (to update the progress) or wait until a final response is retrieved. Agent client should send the intermediate or final response back to the agent for the next run.

Framework Handling: The ADK framework manages the execution. It sends the intermediate or final FunctionResponse sent by agent client to the LLM to generate a user friendly message.

Creating the Tool¶
Define your tool function and wrap it using the LongRunningFunctionTool class:


Python
Go
Java

import (
    "google.golang.org/adk/agent"
    "google.golang.org/adk/agent/llmagent"
    "google.golang.org/adk/model/gemini"
    "google.golang.org/adk/tool"
    "google.golang.org/adk/tool/functiontool"
    "google.golang.org/genai"
)

// CreateTicketArgs defines the arguments for our long-running tool.
type CreateTicketArgs struct {
    Urgency string `json:"urgency" jsonschema:"The urgency level of the ticket."`
}

// CreateTicketResults defines the *initial* output of our long-running tool.
type CreateTicketResults struct {
    Status   string `json:"status"`
    TicketId string `json:"ticket_id"`
}

// createTicketAsync simulates the *initiation* of a long-running ticket creation task.
func createTicketAsync(ctx tool.Context, args CreateTicketArgs) (CreateTicketResults, error) {
    log.Printf("TOOL_EXEC: 'create_ticket_long_running' called with urgency: %s (Call ID: %s)\n", args.Urgency, ctx.FunctionCallID())

    // "Generate" a ticket ID and return it in the initial response.
    ticketID := "TICKET-ABC-123"
    log.Printf("ACTION: Generated Ticket ID: %s for Call ID: %s\n", ticketID, ctx.FunctionCallID())

    // In a real application, you would save the association between the
    // FunctionCallID and the ticketID to handle the async response later.
    return CreateTicketResults{
        Status:   "started",
        TicketId: ticketID,
    }, nil
}

func createTicketAgent(ctx context.Context) (agent.Agent, error) {
    ticketTool, err := functiontool.New(
        functiontool.Config{
            Name:        "create_ticket_long_running",
            Description: "Creates a new support ticket with a specified urgency level.",
        },
        createTicketAsync,
    )
    if err != nil {
        return nil, fmt.Errorf("failed to create long running tool: %w", err)
    }

    model, err := gemini.NewModel(ctx, "gemini-2.5-flash", &genai.ClientConfig{})
    if err != nil {
        return nil, fmt.Errorf("failed to create model: %v", err)
    }

    return llmagent.New(llmagent.Config{
        Name:        "ticket_agent",
        Model:       model,
        Instruction: "You are a helpful assistant for creating support tickets. Provide the status of the ticket at each interaction.",
        Tools:       []tool.Tool{ticketTool},
    })
}

Intermediate / Final result Updates¶
Agent client received an event with long running function calls and check the status of the ticket. Then Agent client can send the intermediate or final response back to update the progress. The framework packages this value (even if it's None) into the content of the FunctionResponse sent back to the LLM.

Note: Long running function response with Resume feature

If your ADK agent workflow is configured with the Resume feature, you also must include the Invocation ID (invocation_id) parameter with the long running function response. The Invocation ID you provide must be the same invocation that generated the long running function request, otherwise the system starts a new invocation with the response. If your agent uses the Resume feature, consider including the Invocation ID as a parameter with your long running function request, so it can be included with the response. For more details on using the Resume feature, see Resume stopped agents.

Applies to only Java ADK

Python
Go
Java
The following example demonstrates a multi-turn workflow. First, the user asks the agent to create a ticket. The agent calls the long-running tool and the client captures the FunctionCall ID. The client then simulates the asynchronous work completing by sending subsequent FunctionResponse messages back to the agent to provide the ticket ID and final status.


// runTurn executes a single turn with the agent and returns the captured function call ID.
func runTurn(ctx context.Context, r *runner.Runner, sessionID, turnLabel string, content *genai.Content) string {
    var funcCallID atomic.Value // Safely store the found ID.

    fmt.Printf("\n--- %s ---\n", turnLabel)
    for event, err := range r.Run(ctx, userID, sessionID, content, agent.RunConfig{
        StreamingMode: agent.StreamingModeNone,
    }) {
        if err != nil {
            fmt.Printf("\nAGENT_ERROR: %v\n", err)
            continue
        }
        // Print a summary of the event for clarity.
        printEventSummary(event, turnLabel)

        // Capture the function call ID from the event.
        for _, part := range event.Content.Parts {
            if fc := part.FunctionCall; fc != nil {
                if fc.Name == "create_ticket_long_running" {
                    funcCallID.Store(fc.ID)
                }
            }
        }
    }

    if id, ok := funcCallID.Load().(string); ok {
        return id
    }
    return ""
}

func main() {
    ctx := context.Background()
    ticketAgent, err := createTicketAgent(ctx)
    if err != nil {
        log.Fatalf("Failed to create agent: %v", err)
    }

    // Setup the runner and session.
    sessionService := session.InMemoryService()
    session, err := sessionService.Create(ctx, &session.CreateRequest{AppName: appName, UserID: userID})
    if err != nil {
        log.Fatalf("Failed to create session: %v", err)
    }
    r, err := runner.New(runner.Config{AppName: appName, Agent: ticketAgent, SessionService: sessionService})
    if err != nil {
        log.Fatalf("Failed to create runner: %v", err)
    }

    // --- Turn 1: User requests to create a ticket. ---
    initialUserMessage := genai.NewContentFromText("Create a high urgency ticket for me.", genai.RoleUser)
    funcCallID := runTurn(ctx, r, session.Session.ID(), "Turn 1: User Request", initialUserMessage)
    if funcCallID == "" {
        log.Fatal("ERROR: Tool 'create_ticket_long_running' not called in Turn 1.")
    }
    fmt.Printf("ACTION: Captured FunctionCall ID: %s\n", funcCallID)

    // --- Turn 2: App provides the final status of the ticket. ---
    // In a real application, the ticketID would be retrieved from a database
    // using the funcCallID. For this example, we'll use the same ID.
    ticketID := "TICKET-ABC-123"
    willContinue := false // Signal that this is the final response.
    ticketStatusResponse := &genai.FunctionResponse{
        Name: "create_ticket_long_running",
        ID:   funcCallID,
        Response: map[string]any{
            "status":    "approved",
            "ticket_id": ticketID,
        },
        WillContinue: &willContinue,
    }
    appResponseWithStatus := &genai.Content{
        Role:  string(genai.RoleUser),
        Parts: []*genai.Part{{FunctionResponse: ticketStatusResponse}},
    }
    runTurn(ctx, r, session.Session.ID(), "Turn 2: App provides ticket status", appResponseWithStatus)
    fmt.Println("Long running function completed successfully.")
}

// printEventSummary provides a readable log of agent and LLM interactions.
func printEventSummary(event *session.Event, turnLabel string) {
    for _, part := range event.Content.Parts {
        // Check for a text part.
        if part.Text != "" {
            fmt.Printf("[%s][%s_TEXT]: %s\n", turnLabel, event.Author, part.Text)
        }
        // Check for a function call part.
        if fc := part.FunctionCall; fc != nil {
            fmt.Printf("[%s][%s_CALL]: %s(%v) ID: %s\n", turnLabel, event.Author, fc.Name, fc.Args, fc.ID)
        }
    }
}

Python complete example: File Processing Simulation
Key aspects of this example¶
LongRunningFunctionTool: Wraps the supplied method/function; the framework handles sending yielded updates and the final return value as sequential FunctionResponses.

Agent instruction: Directs the LLM to use the tool and understand the incoming FunctionResponse stream (progress vs. completion) for user updates.

Final return: The function returns the final result dictionary, which is sent in the concluding FunctionResponse to indicate completion.

Agent-as-a-Tool¶
This powerful feature allows you to leverage the capabilities of other agents within your system by calling them as tools. The Agent-as-a-Tool enables you to invoke another agent to perform a specific task, effectively delegating responsibility. This is conceptually similar to creating a Python function that calls another agent and uses the agent's response as the function's return value.

Key difference from sub-agents¶
It's important to distinguish an Agent-as-a-Tool from a Sub-Agent.

Agent-as-a-Tool: When Agent A calls Agent B as a tool (using Agent-as-a-Tool), Agent B's answer is passed back to Agent A, which then summarizes the answer and generates a response to the user. Agent A retains control and continues to handle future user input.

Sub-agent: When Agent A calls Agent B as a sub-agent, the responsibility of answering the user is completely transferred to Agent B. Agent A is effectively out of the loop. All subsequent user input will be answered by Agent B.

Usage¶
To use an agent as a tool, wrap the agent with the AgentTool class.


Python
Go
Java

agenttool.New(agent, &agenttool.Config{...})

Customization¶
The AgentTool class provides the following attributes for customizing its behavior:

skip_summarization: bool: If set to True, the framework will bypass the LLM-based summarization of the tool agent's response. This can be useful when the tool's response is already well-formatted and requires no further processing.
Example
How it works¶
When the main_agent receives the long text, its instruction tells it to use the 'summarize' tool for long texts.
The framework recognizes 'summarize' as an AgentTool that wraps the summary_agent.
Behind the scenes, the main_agent will call the summary_agent with the long text as input.
The summary_agent will process the text according to its instruction and generate a summary.
The response from the summary_agent is then passed back to the main_agent.
The main_agent can then take the summary and formulate its final response to the user (e.g., "Here's a summary of the text: ...")