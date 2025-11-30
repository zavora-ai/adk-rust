Events¶
Supported in ADKPython v0.1.0Go v0.1.0Java v0.1.0
Events are the fundamental units of information flow within the Agent Development Kit (ADK). They represent every significant occurrence during an agent's interaction lifecycle, from initial user input to the final response and all the steps in between. Understanding events is crucial because they are the primary way components communicate, state is managed, and control flow is directed.

What Events Are and Why They Matter¶
An Event in ADK is an immutable record representing a specific point in the agent's execution. It captures user messages, agent replies, requests to use tools (function calls), tool results, state changes, control signals, and errors.


Python
Go
Java
In Go, this is a struct of type google.golang.org/adk/session.Event.


// Conceptual Structure of an Event (Go - See session/session.go)
// Simplified view based on the session.Event struct
type Event struct {
    // --- Fields from embedded model.LLMResponse ---
    model.LLMResponse

    // --- ADK specific additions ---
    Author       string         // 'user' or agent name
    InvocationID string         // ID for the whole interaction run
    ID           string         // Unique ID for this specific event
    Timestamp    time.Time      // Creation time
    Actions      EventActions   // Important for side-effects & control
    Branch       string         // Hierarchy path
    // ... other fields
}

// model.LLMResponse contains the Content field
type LLMResponse struct {
    Content *genai.Content
    // ... other fields
}

Events are central to ADK's operation for several key reasons:

Communication: They serve as the standard message format between the user interface, the Runner, agents, the LLM, and tools. Everything flows as an Event.

Signaling State & Artifact Changes: Events carry instructions for state modifications and track artifact updates. The SessionService uses these signals to ensure persistence. In Python changes are signaled via event.actions.state_delta and event.actions.artifact_delta.

Control Flow: Specific fields like event.actions.transfer_to_agent or event.actions.escalate act as signals that direct the framework, determining which agent runs next or if a loop should terminate.

History & Observability: The sequence of events recorded in session.events provides a complete, chronological history of an interaction, invaluable for debugging, auditing, and understanding agent behavior step-by-step.

In essence, the entire process, from a user's query to the agent's final answer, is orchestrated through the generation, interpretation, and processing of Event objects.

Understanding and Using Events¶
As a developer, you'll primarily interact with the stream of events yielded by the Runner. Here's how to understand and extract information from them:

Note

The specific parameters or method names for the primitives may vary slightly by SDK language (e.g., event.content() in Python, event.content().get().parts() in Java). Refer to the language-specific API documentation for details.

Identifying Event Origin and Type¶
Quickly determine what an event represents by checking:

Who sent it? (event.author)
'user': Indicates input directly from the end-user.
'AgentName': Indicates output or action from a specific agent (e.g., 'WeatherAgent', 'SummarizerAgent').
What's the main payload? (event.content and event.content.parts)

Text: Indicates a conversational message. For Python, check if event.content.parts[0].text exists. For Java, check if event.content() is present, its parts() are present and not empty, and the first part's text() is present.
Tool Call Request: Check event.get_function_calls(). If not empty, the LLM is asking to execute one or more tools. Each item in the list has .name and .args.
Tool Result: Check event.get_function_responses(). If not empty, this event carries the result(s) from tool execution(s). Each item has .name and .response (the dictionary returned by the tool). Note: For history structuring, the role inside the content is often 'user', but the event author is typically the agent that requested the tool call.
Is it streaming output? (event.partial) Indicates whether this is an incomplete chunk of text from the LLM.

True: More text will follow.
False or None/Optional.empty(): This part of the content is complete (though the overall turn might not be finished if turn_complete is also false).

Python
Go
Java

  // Pseudocode: Basic event identification (Go)
import (
  "fmt"
  "google.golang.org/adk/session"
  "google.golang.org/genai"
)

func hasFunctionCalls(content *genai.Content) bool {
  if content == nil {
    return false
  }
  for _, part := range content.Parts {
    if part.FunctionCall != nil {
      return true
    }
  }
  return false
}

func hasFunctionResponses(content *genai.Content) bool {
  if content == nil {
    return false
  }
  for _, part := range content.Parts {
    if part.FunctionResponse != nil {
      return true
    }
  }
  return false
}

func processEvents(events <-chan *session.Event) {
  for event := range events {
    fmt.Printf("Event from: %s\n", event.Author)

    if event.LLMResponse != nil && event.LLMResponse.Content != nil {
      if hasFunctionCalls(event.LLMResponse.Content) {
        fmt.Println("  Type: Tool Call Request")
      } else if hasFunctionResponses(event.LLMResponse.Content) {
        fmt.Println("  Type: Tool Result")
      } else if len(event.LLMResponse.Content.Parts) > 0 {
        if event.LLMResponse.Content.Parts[0].Text != "" {
          if event.LLMResponse.Partial {
            fmt.Println("  Type: Streaming Text Chunk")
          } else {
            fmt.Println("  Type: Complete Text Message")
          }
        } else {
          fmt.Println("  Type: Other Content (e.g., code result)")
        }
      }
    } else if len(event.Actions.StateDelta) > 0 {
      fmt.Println("  Type: State Update")
    } else {
      fmt.Println("  Type: Control Signal or Other")
    }
  }
}

Extracting Key Information¶
Once you know the event type, access the relevant data:

Text Content: Always check for the presence of content and parts before accessing text. In Python its text = event.content.parts[0].text.

Function Call Details:


Python
Go
Java

import (
    "fmt"
    "google.golang.org/adk/session"
    "google.golang.org/genai"
)

func handleFunctionCalls(event *session.Event) {
    if event.LLMResponse == nil || event.LLMResponse.Content == nil {
        return
    }
    calls := event.Content.FunctionCalls()
    if len(calls) > 0 {
        for _, call := range calls {
            toolName := call.Name
            arguments := call.Args
            fmt.Printf("  Tool: %s, Args: %v\n", toolName, arguments)
            // Application might dispatch execution based on this
        }
    }
}

Function Response Details:


Python
Go
Java

import (
    "fmt"
    "google.golang.org/adk/session"
    "google.golang.org/genai"
)

func handleFunctionResponses(event *session.Event) {
    if event.LLMResponse == nil || event.LLMResponse.Content == nil {
        return
    }
    responses := event.Content.FunctionResponses()
    if len(responses) > 0 {
        for _, response := range responses {
            toolName := response.Name
            result := response.Response
            fmt.Printf("  Tool Result: %s -> %v\n", toolName, result)
        }
    }
}

Identifiers:

event.id: Unique ID for this specific event instance.
event.invocation_id: ID for the entire user-request-to-final-response cycle this event belongs to. Useful for logging and tracing.
Detecting Actions and Side Effects¶
The event.actions object signals changes that occurred or should occur. Always check if event.actions and it's fields/ methods exists before accessing them.

State Changes: Gives you a collection of key-value pairs that were modified in the session state during the step that produced this event.


Python
Go
Java
delta := event.Actions.StateDelta (a map[string]any)


import (
    "fmt"
    "google.golang.org/adk/session"
)

func handleStateChanges(event *session.Event) {
    if len(event.Actions.StateDelta) > 0 {
        fmt.Printf("  State changes: %v\n", event.Actions.StateDelta)
        // Update local UI or application state if necessary
    }
}

Artifact Saves: Gives you a collection indicating which artifacts were saved and their new version number (or relevant Part information).


Python
Go
Java
artifactChanges := event.Actions.ArtifactDelta (a map[string]artifact.Artifact)


import (
    "fmt"
    "google.golang.org/adk/artifact"
    "google.golang.org/adk/session"
)

func handleArtifactChanges(event *session.Event) {
    if len(event.Actions.ArtifactDelta) > 0 {
        fmt.Printf("  Artifacts saved: %v\n", event.Actions.ArtifactDelta)
        // UI might refresh an artifact list
        // Iterate through event.Actions.ArtifactDelta to get filename and artifact.Artifact details
        for filename, art := range event.Actions.ArtifactDelta {
            fmt.Printf("    Filename: %s, Version: %d, MIMEType: %s\n", filename, art.Version, art.MIMEType)
        }
    }
}

Control Flow Signals: Check boolean flags or string values:


Python
Go
Java
event.Actions.TransferToAgent (string): Control should pass to the named agent.
event.Actions.Escalate (bool): A loop should terminate.
event.Actions.SkipSummarization (bool): A tool result should not be summarized by the LLM.

import (
    "fmt"
    "google.golang.org/adk/session"
)

func handleControlFlow(event *session.Event) {
    if event.Actions.TransferToAgent != "" {
        fmt.Printf("  Signal: Transfer to %s\n", event.Actions.TransferToAgent)
    }
    if event.Actions.Escalate {
        fmt.Println("  Signal: Escalate (terminate loop)")
    }
    if event.Actions.SkipSummarization {
        fmt.Println("  Signal: Skip summarization for tool result")
    }
}

Determining if an Event is a "Final" Response¶
Use the built-in helper method event.is_final_response() to identify events suitable for display as the agent's complete output for a turn.

Purpose: Filters out intermediate steps (like tool calls, partial streaming text, internal state updates) from the final user-facing message(s).
When True?
The event contains a tool result (function_response) and skip_summarization is True.
The event contains a tool call (function_call) for a tool marked as is_long_running=True. In Java, check if the longRunningToolIds list is empty:
event.longRunningToolIds().isPresent() && !event.longRunningToolIds().get().isEmpty() is true.
OR, all of the following are met:
No function calls (get_function_calls() is empty).
No function responses (get_function_responses() is empty).
Not a partial stream chunk (partial is not True).
Doesn't end with a code execution result that might need further processing/display.
Usage: Filter the event stream in your application logic.


Python
Go
Java

// Pseudocode: Handling final responses in application (Go)
import (
    "fmt"
    "strings"
    "google.golang.org/adk/session"
    "google.golang.org/genai"
)

// isFinalResponse checks if an event is a final response suitable for display.
func isFinalResponse(event *session.Event) bool {
    if event.LLMResponse != nil {
        // Condition 1: Tool result with skip summarization.
        if event.LLMResponse.Content != nil && len(event.LLMResponse.Content.FunctionResponses()) > 0 && event.Actions.SkipSummarization {
            return true
        }
        // Condition 2: Long-running tool call.
        if len(event.LongRunningToolIDs) > 0 {
            return true
        }
        // Condition 3: A complete message without tool calls or responses.
        if (event.LLMResponse.Content == nil ||
            (len(event.LLMResponse.Content.FunctionCalls()) == 0 && len(event.LLMResponse.Content.FunctionResponses()) == 0)) &&
            !event.LLMResponse.Partial {
            return true
        }
    }
    return false
}

func handleFinalResponses() {
    var fullResponseText strings.Builder
    // for event := range runner.Run(...) { // Example loop
    //  // Accumulate streaming text if needed...
    //  if event.LLMResponse != nil && event.LLMResponse.Partial && event.LLMResponse.Content != nil {
    //      if len(event.LLMResponse.Content.Parts) > 0 && event.LLMResponse.Content.Parts[0].Text != "" {
    //          fullResponseText.WriteString(event.LLMResponse.Content.Parts[0].Text)
    //      }
    //  }
    //
    //  // Check if it's a final, displayable event
    //  if isFinalResponse(event) {
    //      fmt.Println("\n--- Final Output Detected ---")
    //      if event.LLMResponse != nil && event.LLMResponse.Content != nil {
    //          if len(event.LLMResponse.Content.Parts) > 0 && event.LLMResponse.Content.Parts[0].Text != "" {
    //              // If it's the final part of a stream, use accumulated text
    //              finalText := fullResponseText.String()
    //              if !event.LLMResponse.Partial {
    //                  finalText += event.LLMResponse.Content.Parts[0].Text
    //              }
    //              fmt.Printf("Display to user: %s\n", strings.TrimSpace(finalText))
    //              fullResponseText.Reset() // Reset accumulator
    //          }
    //      } else if event.Actions.SkipSummarization && event.LLMResponse.Content != nil && len(event.LLMResponse.Content.FunctionResponses()) > 0 {
    //          // Handle displaying the raw tool result if needed
    //          responseData := event.LLMResponse.Content.FunctionResponses()[0].Response
    //          fmt.Printf("Display raw tool result: %v\n", responseData)
    //      } else if len(event.LongRunningToolIDs) > 0 {
    //          fmt.Println("Display message: Tool is running in background...")
    //      } else {
    //          // Handle other types of final responses if applicable
    //          fmt.Println("Display: Final non-textual response or signal.")
    //      }
    //  }
    // }
}

By carefully examining these aspects of an event, you can build robust applications that react appropriately to the rich information flowing through the ADK system.

How Events Flow: Generation and Processing¶
Events are created at different points and processed systematically by the framework. Understanding this flow helps clarify how actions and history are managed.

Generation Sources:

User Input: The Runner typically wraps initial user messages or mid-conversation inputs into an Event with author='user'.
Agent Logic: Agents (BaseAgent, LlmAgent) explicitly yield Event(...) objects (setting author=self.name) to communicate responses or signal actions.
LLM Responses: The ADK model integration layer translates raw LLM output (text, function calls, errors) into Event objects, authored by the calling agent.
Tool Results: After a tool executes, the framework generates an Event containing the function_response. The author is typically the agent that requested the tool, while the role inside the content is set to 'user' for the LLM history.
Processing Flow:

Yield/Return: An event is generated and yielded (Python) or returned/emitted (Java) by its source.
Runner Receives: The main Runner executing the agent receives the event.
SessionService Processing: The Runner sends the event to the configured SessionService. This is a critical step:
Applies Deltas: The service merges event.actions.state_delta into session.state and updates internal records based on event.actions.artifact_delta. (Note: The actual artifact saving usually happened earlier when context.save_artifact was called).
Finalizes Metadata: Assigns a unique event.id if not present, may update event.timestamp.
Persists to History: Appends the processed event to the session.events list.
External Yield: The Runner yields (Python) or returns/emits (Java) the processed event outwards to the calling application (e.g., the code that invoked runner.run_async).
This flow ensures that state changes and history are consistently recorded alongside the communication content of each event.

Common Event Examples (Illustrative Patterns)¶
Here are concise examples of typical events you might see in the stream:

User Input:

{
  "author": "user",
  "invocation_id": "e-xyz...",
  "content": {"parts": [{"text": "Book a flight to London for next Tuesday"}]}
  // actions usually empty
}
Agent Final Text Response: (is_final_response() == True)

{
  "author": "TravelAgent",
  "invocation_id": "e-xyz...",
  "content": {"parts": [{"text": "Okay, I can help with that. Could you confirm the departure city?"}]},
  "partial": false,
  "turn_complete": true
  // actions might have state delta, etc.
}
Agent Streaming Text Response: (is_final_response() == False)

{
  "author": "SummaryAgent",
  "invocation_id": "e-abc...",
  "content": {"parts": [{"text": "The document discusses three main points:"}]},
  "partial": true,
  "turn_complete": false
}
// ... more partial=True events follow ...
Tool Call Request (by LLM): (is_final_response() == False)

{
  "author": "TravelAgent",
  "invocation_id": "e-xyz...",
  "content": {"parts": [{"function_call": {"name": "find_airports", "args": {"city": "London"}}}]}
  // actions usually empty
}
Tool Result Provided (to LLM): (is_final_response() depends on skip_summarization)

{
  "author": "TravelAgent", // Author is agent that requested the call
  "invocation_id": "e-xyz...",
  "content": {
    "role": "user", // Role for LLM history
    "parts": [{"function_response": {"name": "find_airports", "response": {"result": ["LHR", "LGW", "STN"]}}}]
  }
  // actions might have skip_summarization=True
}
State/Artifact Update Only: (is_final_response() == False)

{
  "author": "InternalUpdater",
  "invocation_id": "e-def...",
  "content": null,
  "actions": {
    "state_delta": {"user_status": "verified"},
    "artifact_delta": {"verification_doc.pdf": 2}
  }
}
Agent Transfer Signal: (is_final_response() == False)

{
  "author": "OrchestratorAgent",
  "invocation_id": "e-789...",
  "content": {"parts": [{"function_call": {"name": "transfer_to_agent", "args": {"agent_name": "BillingAgent"}}}]},
  "actions": {"transfer_to_agent": "BillingAgent"} // Added by framework
}
Loop Escalation Signal: (is_final_response() == False)

{
  "author": "CheckerAgent",
  "invocation_id": "e-loop...",
  "content": {"parts": [{"text": "Maximum retries reached."}]}, // Optional content
  "actions": {"escalate": true}
}
Additional Context and Event Details¶
Beyond the core concepts, here are a few specific details about context and events that are important for certain use cases:

ToolContext.function_call_id (Linking Tool Actions):

When an LLM requests a tool (FunctionCall), that request has an ID. The ToolContext provided to your tool function includes this function_call_id.
Importance: This ID is crucial for linking actions like authentication back to the specific tool request that initiated them, especially if multiple tools are called in one turn. The framework uses this ID internally.
How State/Artifact Changes are Recorded:

When you modify state or save an artifact using CallbackContext or ToolContext, these changes aren't immediately written to persistent storage.
Instead, they populate the state_delta and artifact_delta fields within the EventActions object.
This EventActions object is attached to the next event generated after the change (e.g., the agent's response or a tool result event).
The SessionService.append_event method reads these deltas from the incoming event and applies them to the session's persistent state and artifact records. This ensures changes are tied chronologically to the event stream.
State Scope Prefixes (app:, user:, temp:):

When managing state via context.state, you can optionally use prefixes:
app:my_setting: Suggests state relevant to the entire application (requires a persistent SessionService).
user:user_preference: Suggests state relevant to the specific user across sessions (requires a persistent SessionService).
temp:intermediate_result or no prefix: Typically session-specific or temporary state for the current invocation.
The underlying SessionService determines how these prefixes are handled for persistence.
Error Events:

An Event can represent an error. Check the event.error_code and event.error_message fields (inherited from LlmResponse).
Errors might originate from the LLM (e.g., safety filters, resource limits) or potentially be packaged by the framework if a tool fails critically. Check tool FunctionResponse content for typical tool-specific errors.

// Example Error Event (conceptual)
{
  "author": "LLMAgent",
  "invocation_id": "e-err...",
  "content": null,
  "error_code": "SAFETY_FILTER_TRIGGERED",
  "error_message": "Response blocked due to safety settings.",
  "actions": {}
}
These details provide a more complete picture for advanced use cases involving tool authentication, state persistence scope, and error handling within the event stream.

Best Practices for Working with Events¶
To use events effectively in your ADK applications:

Clear Authorship: When building custom agents, ensure correct attribution for agent actions in the history. The framework generally handles authorship correctly for LLM/tool events.


Python
Go
Java
In custom agent Run methods, the framework typically handles authorship. If creating an event manually, set the author: yield(&session.Event{Author: a.name, ...}, nil)


Semantic Content & Actions: Use event.content for the core message/data (text, function call/response). Use event.actions specifically for signaling side effects (state/artifact deltas) or control flow (transfer, escalate, skip_summarization).

Idempotency Awareness: Understand that the SessionService is responsible for applying the state/artifact changes signaled in event.actions. While ADK services aim for consistency, consider potential downstream effects if your application logic re-processes events.
Use is_final_response(): Rely on this helper method in your application/UI layer to identify complete, user-facing text responses. Avoid manually replicating its logic.
Leverage History: The session's event list is your primary debugging tool. Examine the sequence of authors, content, and actions to trace execution and diagnose issues.
Use Metadata: Use invocation_id to correlate all events within a single user interaction. Use event.id to reference specific, unique occurrences.
Treating events as structured messages with clear purposes for their content and actions is key to building, debugging, and managing complex agent behaviors in ADK