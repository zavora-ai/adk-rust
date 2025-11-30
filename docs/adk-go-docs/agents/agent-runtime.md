Runtime¶
Supported in ADKPython v0.1.0Go v0.1.0Java v0.1.0
The ADK Runtime is the underlying engine that powers your agent application during user interactions. It's the system that takes your defined agents, tools, and callbacks and orchestrates their execution in response to user input, managing the flow of information, state changes, and interactions with external services like LLMs or storage.

Think of the Runtime as the "engine" of your agentic application. You define the parts (agents, tools), and the Runtime handles how they connect and run together to fulfill a user's request.

Core Idea: The Event Loop¶
At its heart, the ADK Runtime operates on an Event Loop. This loop facilitates a back-and-forth communication between the Runner component and your defined "Execution Logic" (which includes your Agents, the LLM calls they make, Callbacks, and Tools).

intro_components.png

In simple terms:

The Runner receives a user query and asks the main Agent to start processing.
The Agent (and its associated logic) runs until it has something to report (like a response, a request to use a tool, or a state change) – it then yields or emits an Event.
The Runner receives this Event, processes any associated actions (like saving state changes via Services), and forwards the event onwards (e.g., to the user interface).
Only after the Runner has processed the event does the Agent's logic resume from where it paused, now potentially seeing the effects of the changes committed by the Runner.
This cycle repeats until the agent has no more events to yield for the current user query.
This event-driven loop is the fundamental pattern governing how ADK executes your agent code.

The Heartbeat: The Event Loop - Inner workings¶
The Event Loop is the core operational pattern defining the interaction between the Runner and your custom code (Agents, Tools, Callbacks, collectively referred to as "Execution Logic" or "Logic Components" in the design document). It establishes a clear division of responsibilities:

Note

The specific method names and parameter names may vary slightly by SDK language (e.g., agent_to_run.run_async(...) in Python, agent.Run(...) in Go, agent_to_run.runAsync(...) in Java ). Refer to the language-specific API documentation for details.

Runner's Role (Orchestrator)¶
The Runner acts as the central coordinator for a single user invocation. Its responsibilities in the loop are:

Initiation: Receives the end user's query (new_message) and typically appends it to the session history via the SessionService.
Kick-off: Starts the event generation process by calling the main agent's execution method (e.g., agent_to_run.run_async(...)).
Receive & Process: Waits for the agent logic to yield or emit an Event. Upon receiving an event, the Runner promptly processes it. This involves:
Using configured Services (SessionService, ArtifactService, MemoryService) to commit changes indicated in event.actions (like state_delta, artifact_delta).
Performing other internal bookkeeping.
Yield Upstream: Forwards the processed event onwards (e.g., to the calling application or UI for rendering).
Iterate: Signals the agent logic that processing is complete for the yielded event, allowing it to resume and generate the next event.
Conceptual Runner Loop:


Python
Go
Java

// Simplified conceptual view of the Runner's main loop logic in Go
func (r *Runner) RunConceptual(ctx context.Context, session *session.Session, newQuery *genai.Content) iter.Seq2[*Event, error] {
    return func(yield func(*Event, error) bool) {
        // 1. Append new_query to session event history (via SessionService)
        // ...
        userEvent := session.NewEvent(ctx.InvocationID()) // Simplified for conceptual view
        userEvent.Author = "user"
        userEvent.LLMResponse = model.LLMResponse{Content: newQuery}

        if _, err := r.sessionService.Append(ctx, &session.AppendRequest{Event: userEvent}); err != nil {
            yield(nil, err)
            return
        }

        // 2. Kick off event stream by calling the agent
        // Assuming agent.Run also returns iter.Seq2[*Event, error]
        agentEventsAndErrs := r.agent.Run(ctx, &agent.RunRequest{Session: session, Input: newQuery})

        for event, err := range agentEventsAndErrs {
            if err != nil {
                if !yield(event, err) { // Yield event even if there's an error, then stop
                    return
                }
                return // Agent finished with an error
            }

            // 3. Process the generated event and commit changes
            // Only commit non-partial event to a session service (as seen in actual code)
            if !event.LLMResponse.Partial {
                if _, err := r.sessionService.Append(ctx, &session.AppendRequest{Event: event}); err != nil {
                    yield(nil, err)
                    return
                }
            }
            // memory_service.update_memory(...) // If applicable
            // artifact_service might have already been called via context during agent run

            // 4. Yield event for upstream processing
            if !yield(event, nil) {
                return // Upstream consumer stopped
            }
        }
        // Agent finished successfully
    }
}

Execution Logic's Role (Agent, Tool, Callback)¶
Your code within agents, tools, and callbacks is responsible for the actual computation and decision-making. Its interaction with the loop involves:

Execute: Runs its logic based on the current InvocationContext, including the session state as it was when execution resumed.
Yield: When the logic needs to communicate (send a message, call a tool, report a state change), it constructs an Event containing the relevant content and actions, and then yields this event back to the Runner.
Pause: Crucially, execution of the agent logic pauses immediately after the yield statement (or return in RxJava). It waits for the Runner to complete step 3 (processing and committing).
Resume: Only after the Runner has processed the yielded event does the agent logic resume execution from the statement immediately following the yield.
See Updated State: Upon resumption, the agent logic can now reliably access the session state (ctx.session.state) reflecting the changes that were committed by the Runner from the previously yielded event.
Conceptual Execution Logic:


Python
Go
Java

// Simplified view of logic inside Agent.Run, callbacks, or tools

// ... previous code runs based on current state ...

// 1. Determine a change or output is needed, construct the event
// Example: Updating state
updateData := map[string]interface{}{"field_1": "value_2"}
eventWithStateChange := &Event{
    Author: self.Name(),
    Actions: &EventActions{StateDelta: updateData},
    Content: genai.NewContentFromText("State updated.", "model"),
    // ... other event fields ...
}

// 2. Yield the event to the Runner for processing & commit
// In Go, this is done by sending the event to a channel.
eventsChan <- eventWithStateChange
// <<<<<<<<<<<< EXECUTION PAUSES HERE (conceptually) >>>>>>>>>>>>
// The Runner on the other side of the channel will receive and process the event.
// The agent's goroutine might continue, but the logical flow waits for the next input or step.

// <<<<<<<<<<<< RUNNER PROCESSES & COMMITS THE EVENT >>>>>>>>>>>>

// 3. Resume execution ONLY after Runner is done processing the above event.
// In a real Go implementation, this would likely be handled by the agent receiving
// a new RunRequest or context indicating the next step. The updated state
// would be part of the session object in that new request.
// For this conceptual example, we'll just check the state.
val := ctx.State.Get("field_1")
// here `val` is guaranteed to be "value_2" because the Runner would have
// updated the session state before calling the agent again.
fmt.Printf("Resumed execution. Value of field_1 is now: %v\n", val)

// ... subsequent code continues ...
// Maybe send another event to the channel later...

This cooperative yield/pause/resume cycle between the Runner and your Execution Logic, mediated by Event objects, forms the core of the ADK Runtime.

Key components of the Runtime¶
Several components work together within the ADK Runtime to execute an agent invocation. Understanding their roles clarifies how the event loop functions:

Runner¶
Role: The main entry point and orchestrator for a single user query (run_async).
Function: Manages the overall Event Loop, receives events yielded by the Execution Logic, coordinates with Services to process and commit event actions (state/artifact changes), and forwards processed events upstream (e.g., to the UI). It essentially drives the conversation turn by turn based on yielded events. (Defined in google.adk.runners.runner).
Execution Logic Components¶
Role: The parts containing your custom code and the core agent capabilities.
Components:
Agent (BaseAgent, LlmAgent, etc.): Your primary logic units that process information and decide on actions. They implement the _run_async_impl method which yields events.
Tools (BaseTool, FunctionTool, AgentTool, etc.): External functions or capabilities used by agents (often LlmAgent) to interact with the outside world or perform specific tasks. They execute and return results, which are then wrapped in events.
Callbacks (Functions): User-defined functions attached to agents (e.g., before_agent_callback, after_model_callback) that hook into specific points in the execution flow, potentially modifying behavior or state, whose effects are captured in events.
Function: Perform the actual thinking, calculation, or external interaction. They communicate their results or needs by yielding Event objects and pausing until the Runner processes them.
Event¶
Role: The message passed back and forth between the Runner and the Execution Logic.
Function: Represents an atomic occurrence (user input, agent text, tool call/result, state change request, control signal). It carries both the content of the occurrence and the intended side effects (actions like state_delta).
Services¶
Role: Backend components responsible for managing persistent or shared resources. Used primarily by the Runner during event processing.
Components:
SessionService (BaseSessionService, InMemorySessionService, etc.): Manages Session objects, including saving/loading them, applying state_delta to the session state, and appending events to the event history.
ArtifactService (BaseArtifactService, InMemoryArtifactService, GcsArtifactService, etc.): Manages the storage and retrieval of binary artifact data. Although save_artifact is called via context during execution logic, the artifact_delta in the event confirms the action for the Runner/SessionService.
MemoryService (BaseMemoryService, etc.): (Optional) Manages long-term semantic memory across sessions for a user.
Function: Provide the persistence layer. The Runner interacts with them to ensure changes signaled by event.actions are reliably stored before the Execution Logic resumes.
Session¶
Role: A data container holding the state and history for one specific conversation between a user and the application.
Function: Stores the current state dictionary, the list of all past events (event history), and references to associated artifacts. It's the primary record of the interaction, managed by the SessionService.
Invocation¶
Role: A conceptual term representing everything that happens in response to a single user query, from the moment the Runner receives it until the agent logic finishes yielding events for that query.
Function: An invocation might involve multiple agent runs (if using agent transfer or AgentTool), multiple LLM calls, tool executions, and callback executions, all tied together by a single invocation_id within the InvocationContext. State variables prefixed with temp: are strictly scoped to a single invocation and discarded afterwards.
These players interact continuously through the Event Loop to process a user's request.

How It Works: A Simplified Invocation¶
Let's trace a simplified flow for a typical user query that involves an LLM agent calling a tool:

intro_components.png

Step-by-Step Breakdown¶
User Input: The User sends a query (e.g., "What's the capital of France?").
Runner Starts: Runner.run_async begins. It interacts with the SessionService to load the relevant Session and adds the user query as the first Event to the session history. An InvocationContext (ctx) is prepared.
Agent Execution: The Runner calls agent.run_async(ctx) on the designated root agent (e.g., an LlmAgent).
LLM Call (Example): The Agent_Llm determines it needs information, perhaps by calling a tool. It prepares a request for the LLM. Let's assume the LLM decides to call MyTool.
Yield FunctionCall Event: The Agent_Llm receives the FunctionCall response from the LLM, wraps it in an Event(author='Agent_Llm', content=Content(parts=[Part(function_call=...)])), and yields or emits this event.
Agent Pauses: The Agent_Llm's execution pauses immediately after the yield.
Runner Processes: The Runner receives the FunctionCall event. It passes it to the SessionService to record it in the history. The Runner then yields the event upstream to the User (or application).
Agent Resumes: The Runner signals that the event is processed, and Agent_Llm resumes execution.
Tool Execution: The Agent_Llm's internal flow now proceeds to execute the requested MyTool. It calls tool.run_async(...).
Tool Returns Result: MyTool executes and returns its result (e.g., {'result': 'Paris'}).
Yield FunctionResponse Event: The agent (Agent_Llm) wraps the tool result into an Event containing a FunctionResponse part (e.g., Event(author='Agent_Llm', content=Content(role='user', parts=[Part(function_response=...)]))). This event might also contain actions if the tool modified state (state_delta) or saved artifacts (artifact_delta). The agent yields this event.
Agent Pauses: Agent_Llm pauses again.
Runner Processes: Runner receives the FunctionResponse event. It passes it to SessionService which applies any state_delta/artifact_delta and adds the event to history. Runner yields the event upstream.
Agent Resumes: Agent_Llm resumes, now knowing the tool result and any state changes are committed.
Final LLM Call (Example): Agent_Llm sends the tool result back to the LLM to generate a natural language response.
Yield Final Text Event: Agent_Llm receives the final text from the LLM, wraps it in an Event(author='Agent_Llm', content=Content(parts=[Part(text=...)])), and yields it.
Agent Pauses: Agent_Llm pauses.
Runner Processes: Runner receives the final text event, passes it to SessionService for history, and yields it upstream to the User. This is likely marked as the is_final_response().
Agent Resumes & Finishes: Agent_Llm resumes. Having completed its task for this invocation, its run_async generator finishes.
Runner Completes: The Runner sees the agent's generator is exhausted and finishes its loop for this invocation.
This yield/pause/process/resume cycle ensures that state changes are consistently applied and that the execution logic always operates on the most recently committed state after yielding an event.

Important Runtime Behaviors¶
Understanding a few key aspects of how the ADK Runtime handles state, streaming, and asynchronous operations is crucial for building predictable and efficient agents.

State Updates & Commitment Timing¶
The Rule: When your code (in an agent, tool, or callback) modifies the session state (e.g., context.state['my_key'] = 'new_value'), this change is initially recorded locally within the current InvocationContext. The change is only guaranteed to be persisted (saved by the SessionService) after the Event carrying the corresponding state_delta in its actions has been yield-ed by your code and subsequently processed by the Runner.

Implication: Code that runs after resuming from a yield can reliably assume that the state changes signaled in the yielded event have been committed.


Python
Go
Java

  // Inside agent logic (conceptual)

func (a *Agent) RunConceptual(ctx agent.InvocationContext) iter.Seq2[*session.Event, error] {
  // The entire logic is wrapped in a function that will be returned as an iterator.
  return func(yield func(*session.Event, error) bool) {
      // ... previous code runs based on current state from the input `ctx` ...
      // e.g., val := ctx.State().Get("field_1") might return "value_1" here.

      // 1. Determine a change or output is needed, construct the event
      updateData := map[string]interface{}{"field_1": "value_2"}
      eventWithStateChange := session.NewEvent(ctx.InvocationID())
      eventWithStateChange.Author = a.Name()
      eventWithStateChange.Actions = &session.EventActions{StateDelta: updateData}
      // ... other event fields ...


      // 2. Yield the event to the Runner for processing & commit.
      // The agent's execution continues immediately after this call.
      if !yield(eventWithStateChange, nil) {
          // If yield returns false, it means the consumer (the Runner)
          // has stopped listening, so we should stop producing events.
          return
      }

      // <<<<<<<<<<<< RUNNER PROCESSES & COMMITS THE EVENT >>>>>>>>>>>>
      // This happens outside the agent, after the agent's iterator has
      // produced the event.

      // 3. The agent CANNOT immediately see the state change it just yielded.
      // The state is immutable within a single `Run` invocation.
      val := ctx.State().Get("field_1")
      // `val` here is STILL "value_1" (or whatever it was at the start).
      // The updated state ("value_2") will only be available in the `ctx`
      // of the *next* `Run` invocation in a subsequent turn.

      // ... subsequent code continues, potentially yielding more events ...
      finalEvent := session.NewEvent(ctx.InvocationID())
      finalEvent.Author = a.Name()
      // ...
      yield(finalEvent, nil)
  }
}

"Dirty Reads" of Session State¶
Definition: While commitment happens after the yield, code running later within the same invocation, but before the state-changing event is actually yielded and processed, can often see the local, uncommitted changes. This is sometimes called a "dirty read".
Example:

Python
Go
Java

// Code in before_agent_callback
// The callback would modify the context's session state directly.
// This change is local to the current invocation context.
ctx.State.Set("field_1", "value_1")
// State is locally set to 'value_1', but not yet committed by Runner

// ... agent runs ...

// Code in a tool called later *within the same invocation*
// Readable (dirty read), but 'value_1' isn't guaranteed persistent yet.
val := ctx.State.Get("field_1") // 'val' will likely be 'value_1' here
fmt.Printf("Dirty read value in tool: %v\n", val)

// Assume the event carrying the state_delta={'field_1': 'value_1'}
// is yielded *after* this tool runs and is processed by the Runner.

Implications:
Benefit: Allows different parts of your logic within a single complex step (e.g., multiple callbacks or tool calls before the next LLM turn) to coordinate using state without waiting for a full yield/commit cycle.
Caveat: Relying heavily on dirty reads for critical logic can be risky. If the invocation fails before the event carrying the state_delta is yielded and processed by the Runner, the uncommitted state change will be lost. For critical state transitions, ensure they are associated with an event that gets successfully processed.
Streaming vs. Non-Streaming Output (partial=True)¶
This primarily relates to how responses from the LLM are handled, especially when using streaming generation APIs.

Streaming: The LLM generates its response token-by-token or in small chunks.
The framework (often within BaseLlmFlow) yields multiple Event objects for a single conceptual response. Most of these events will have partial=True.
The Runner, upon receiving an event with partial=True, typically forwards it immediately upstream (for UI display) but skips processing its actions (like state_delta).
Eventually, the framework yields a final event for that response, marked as non-partial (partial=False or implicitly via turn_complete=True).
The Runner fully processes only this final event, committing any associated state_delta or artifact_delta.
Non-Streaming: The LLM generates the entire response at once. The framework yields a single event marked as non-partial, which the Runner processes fully.
Why it Matters: Ensures that state changes are applied atomically and only once based on the complete response from the LLM, while still allowing the UI to display text progressively as it's generated.
Async is Primary (run_async)¶
Core Design: The ADK Runtime is fundamentally built on asynchronous libraries (like Python's asyncio and Java's RxJava) to handle concurrent operations (like waiting for LLM responses or tool executions) efficiently without blocking.
Main Entry Point: Runner.run_async is the primary method for executing agent invocations. All core runnable components (Agents, specific flows) use asynchronous methods internally.
Synchronous Convenience (run): A synchronous Runner.run method exists mainly for convenience (e.g., in simple scripts or testing environments). However, internally, Runner.run typically just calls Runner.run_async and manages the async event loop execution for you.
Developer Experience: We recommend designing your applications (e.g., web servers using ADK) to be asynchronous for best performance. In Python, this means using asyncio; in Java, leverage RxJava's reactive programming model.
Sync Callbacks/Tools: The ADK framework supports both asynchronous and synchronous functions for tools and callbacks.
Blocking I/O: For long-running synchronous I/O operations, the framework attempts to prevent stalls. Python ADK may use asyncio.to_thread, while Java ADK often relies on appropriate RxJava schedulers or wrappers for blocking calls.
CPU-Bound Work: Purely CPU-intensive synchronous tasks will still block their execution thread in both environments.
Understanding these behaviors helps you write more robust ADK applications and debug issues related to state consistency, streaming updates, and asynchronous execution.