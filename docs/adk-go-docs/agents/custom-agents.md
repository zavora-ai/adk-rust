Custom agents¶
Supported in ADKPython v0.1.0Go v0.1.0Java v0.2.0
Custom agents provide the ultimate flexibility in ADK, allowing you to define arbitrary orchestration logic by inheriting directly from BaseAgent and implementing your own control flow. This goes beyond the predefined patterns of SequentialAgent, LoopAgent, and ParallelAgent, enabling you to build highly specific and complex agentic workflows.

Advanced Concept

Building custom agents by directly implementing _run_async_impl (or its equivalent in other languages) provides powerful control but is more complex than using the predefined LlmAgent or standard WorkflowAgent types. We recommend understanding those foundational agent types first before tackling custom orchestration logic.

Introduction: Beyond Predefined Workflows¶
What is a Custom Agent?¶
A Custom Agent is essentially any class you create that inherits from google.adk.agents.BaseAgent and implements its core execution logic within the _run_async_impl asynchronous method. You have complete control over how this method calls other agents (sub-agents), manages state, and handles events.

Note

The specific method name for implementing an agent's core asynchronous logic may vary slightly by SDK language (e.g., runAsyncImpl in Java, _run_async_impl in Python). Refer to the language-specific API documentation for details.

Why Use Them?¶
While the standard Workflow Agents (SequentialAgent, LoopAgent, ParallelAgent) cover common orchestration patterns, you'll need a Custom agent when your requirements include:

Conditional Logic: Executing different sub-agents or taking different paths based on runtime conditions or the results of previous steps.
Complex State Management: Implementing intricate logic for maintaining and updating state throughout the workflow beyond simple sequential passing.
External Integrations: Incorporating calls to external APIs, databases, or custom libraries directly within the orchestration flow control.
Dynamic Agent Selection: Choosing which sub-agent(s) to run next based on dynamic evaluation of the situation or input.
Unique Workflow Patterns: Implementing orchestration logic that doesn't fit the standard sequential, parallel, or loop structures.
intro_components.png

Implementing Custom Logic:¶
The core of any custom agent is the method where you define its unique asynchronous behavior. This method allows you to orchestrate sub-agents and manage the flow of execution.


Python
Go
Java
In Go, you implement the Run method as part of a struct that satisfies the agent.Agent interface. The actual logic is typically a method on your custom agent struct.

Signature: Run(ctx agent.InvocationContext) iter.Seq2[*session.Event, error]
Iterator: The Run method returns an iterator (iter.Seq2) that yields events and errors. This is the standard way to handle streaming results from an agent's execution.
ctx (InvocationContext): The agent.InvocationContext provides access to the session, including state, and other crucial runtime information.
Session State: You can access the session state through ctx.Session().State().

Key Capabilities within the Core Asynchronous Method:


Python
Go
Java
Calling Sub-Agents: You invoke sub-agents by calling their Run method.


// Example: Running one sub-agent and yielding its events
for event, err := range someSubAgent.Run(ctx) {
    if err != nil {
        // Handle or propagate the error
        return
    }
    // Yield the event up to the caller
    if !yield(event, nil) {
      return
    }
}
Managing State: Read from and write to the session state to pass data between sub-agent calls or make decisions.


// The `ctx` (`agent.InvocationContext`) is passed directly to your agent's `Run` function.
// Read data set by a previous agent
previousResult, err := ctx.Session().State().Get("some_key")
if err != nil {
    // Handle cases where the key might not exist yet
}

// Make a decision based on state
if val, ok := previousResult.(string); ok && val == "some_value" {
    // ... call a specific sub-agent ...
} else {
    // ... call another sub-agent ...
}

// Store a result for a later step
if err := ctx.Session().State().Set("my_custom_result", "calculated_value"); err != nil {
    // Handle error
}
Implementing Control Flow: Use standard Go constructs (if/else, for/switch loops, goroutines, channels) to create sophisticated, conditional, or iterative workflows involving your sub-agents.


Managing Sub-Agents and State¶
Typically, a custom agent orchestrates other agents (like LlmAgent, LoopAgent, etc.).

Initialization: You usually pass instances of these sub-agents into your custom agent's constructor and store them as instance fields/attributes (e.g., this.story_generator = story_generator_instance or self.story_generator = story_generator_instance). This makes them accessible within the custom agent's core asynchronous execution logic (such as: _run_async_impl method).
Sub Agents List: When initializing the BaseAgent using it's super() constructor, you should pass a sub agents list. This list tells the ADK framework about the agents that are part of this custom agent's immediate hierarchy. It's important for framework features like lifecycle management, introspection, and potentially future routing capabilities, even if your core execution logic (_run_async_impl) calls the agents directly via self.xxx_agent. Include the agents that your custom logic directly invokes at the top level.
State: As mentioned, ctx.session.state is the standard way sub-agents (especially LlmAgents using output key) communicate results back to the orchestrator and how the orchestrator passes necessary inputs down.
Design Pattern Example: StoryFlowAgent¶
Let's illustrate the power of custom agents with an example pattern: a multi-stage content generation workflow with conditional logic.

Goal: Create a system that generates a story, iteratively refines it through critique and revision, performs final checks, and crucially, regenerates the story if the final tone check fails.

Why Custom? The core requirement driving the need for a custom agent here is the conditional regeneration based on the tone check. Standard workflow agents don't have built-in conditional branching based on the outcome of a sub-agent's task. We need custom logic (if tone == "negative": ...) within the orchestrator.

Part 1: Simplified custom agent Initialization¶

Python
Go
Java
We define the StoryFlowAgent struct and a constructor. In the constructor, we store the necessary sub-agents and tell the BaseAgent framework about the top-level agents this custom agent will directly orchestrate.


// StoryFlowAgent is a custom agent that orchestrates a story generation workflow.
// It encapsulates the logic of running sub-agents in a specific sequence.
type StoryFlowAgent struct {
    storyGenerator     agent.Agent
    revisionLoopAgent  agent.Agent
    postProcessorAgent agent.Agent
}

// NewStoryFlowAgent creates and configures the entire custom agent workflow.
// It takes individual LLM agents as input and internally creates the necessary
// workflow agents (loop, sequential), returning the final orchestrator agent.
func NewStoryFlowAgent(
    storyGenerator,
    critic,
    reviser,
    grammarCheck,
    toneCheck agent.Agent,
) (agent.Agent, error) {
    loopAgent, err := loopagent.New(loopagent.Config{
        MaxIterations: 2,
        AgentConfig: agent.Config{
            Name:      "CriticReviserLoop",
            SubAgents: []agent.Agent{critic, reviser},
        },
    })
    if err != nil {
        return nil, fmt.Errorf("failed to create loop agent: %w", err)
    }

    sequentialAgent, err := sequentialagent.New(sequentialagent.Config{
        AgentConfig: agent.Config{
            Name:      "PostProcessing",
            SubAgents: []agent.Agent{grammarCheck, toneCheck},
        },
    })
    if err != nil {
        return nil, fmt.Errorf("failed to create sequential agent: %w", err)
    }

    // The StoryFlowAgent struct holds the agents needed for the Run method.
    orchestrator := &StoryFlowAgent{
        storyGenerator:     storyGenerator,
        revisionLoopAgent:  loopAgent,
        postProcessorAgent: sequentialAgent,
    }

    // agent.New creates the final agent, wiring up the Run method.
    return agent.New(agent.Config{
        Name:        "StoryFlowAgent",
        Description: "Orchestrates story generation, critique, revision, and checks.",
        SubAgents:   []agent.Agent{storyGenerator, loopAgent, sequentialAgent},
        Run:         orchestrator.Run,
    })
}

Part 2: Defining the Custom Execution Logic¶

Python
Go
Java
The Run method orchestrates the sub-agents by calling their respective Run methods in a loop and yielding their events.


// Run defines the custom execution logic for the StoryFlowAgent.
func (s *StoryFlowAgent) Run(ctx agent.InvocationContext) iter.Seq2[*session.Event, error] {
    return func(yield func(*session.Event, error) bool) {
        // Stage 1: Initial Story Generation
        for event, err := range s.storyGenerator.Run(ctx) {
            if err != nil {
                yield(nil, fmt.Errorf("story generator failed: %w", err))
                return
            }
            if !yield(event, nil) {
                return
            }
        }

        // Check if story was generated before proceeding
        currentStory, err := ctx.Session().State().Get("current_story")
        if err != nil || currentStory == "" {
            log.Println("Failed to generate initial story. Aborting workflow.")
            return
        }

        // Stage 2: Critic-Reviser Loop
        for event, err := range s.revisionLoopAgent.Run(ctx) {
            if err != nil {
                yield(nil, fmt.Errorf("loop agent failed: %w", err))
                return
            }
            if !yield(event, nil) {
                return
            }
        }

        // Stage 3: Post-Processing
        for event, err := range s.postProcessorAgent.Run(ctx) {
            if err != nil {
                yield(nil, fmt.Errorf("sequential agent failed: %w", err))
                return
            }
            if !yield(event, nil) {
                return
            }
        }

        // Stage 4: Conditional Regeneration
        toneResult, err := ctx.Session().State().Get("tone_check_result")
        if err != nil {
            log.Printf("Could not read tone_check_result from state: %v. Assuming tone is not negative.", err)
            return
        }

        if tone, ok := toneResult.(string); ok && tone == "negative" {
            log.Println("Tone is negative. Regenerating story...")
            for event, err := range s.storyGenerator.Run(ctx) {
                if err != nil {
                    yield(nil, fmt.Errorf("story regeneration failed: %w", err))
                    return
                }
                if !yield(event, nil) {
                    return
                }
            }
        } else {
            log.Println("Tone is not negative. Keeping current story.")
        }
    }
}
Explanation of Logic:
The initial storyGenerator runs. Its output is expected to be in the session state under the key "current_story".
The revisionLoopAgent runs, which internally calls the critic and reviser sequentially for max_iterations times. They read/write current_story and criticism from/to the state.
The postProcessorAgent runs, calling grammar_check then tone_check, reading current_story and writing grammar_suggestions and tone_check_result to the state.
Custom Part: The code checks the tone_check_result from the state. If it's "negative", the story_generator is called again, overwriting the current_story in the state. Otherwise, the flow ends.

Part 3: Defining the LLM Sub-Agents¶
These are standard LlmAgent definitions, responsible for specific tasks. Their output key parameter is crucial for placing results into the session.state where other agents or the custom orchestrator can access them.

Direct State Injection in Instructions

Notice the story_generator's instruction. The {var} syntax is a placeholder. Before the instruction is sent to the LLM, the ADK framework automatically replaces (Example:{topic}) with the value of session.state['topic']. This is the recommended way to provide context to an agent, using templating in the instructions. For more details, see the State documentation.


Python
Java
Go

// --- Define the individual LLM agents ---
storyGenerator, err := llmagent.New(llmagent.Config{
    Name:        "StoryGenerator",
    Model:       model,
    Description: "Generates the initial story.",
    Instruction: "You are a story writer. Write a short story (around 100 words) about a cat, based on the topic: {topic}",
    OutputKey:   "current_story",
})
if err != nil {
    log.Fatalf("Failed to create StoryGenerator agent: %v", err)
}

critic, err := llmagent.New(llmagent.Config{
    Name:        "Critic",
    Model:       model,
    Description: "Critiques the story.",
    Instruction: "You are a story critic. Review the story: {current_story}. Provide 1-2 sentences of constructive criticism on how to improve it. Focus on plot or character.",
    OutputKey:   "criticism",
})
if err != nil {
    log.Fatalf("Failed to create Critic agent: %v", err)
}

reviser, err := llmagent.New(llmagent.Config{
    Name:        "Reviser",
    Model:       model,
    Description: "Revises the story based on criticism.",
    Instruction: "You are a story reviser. Revise the story: {current_story}, based on the criticism: {criticism}. Output only the revised story.",
    OutputKey:   "current_story",
})
if err != nil {
    log.Fatalf("Failed to create Reviser agent: %v", err)
}

grammarCheck, err := llmagent.New(llmagent.Config{
    Name:        "GrammarCheck",
    Model:       model,
    Description: "Checks grammar and suggests corrections.",
    Instruction: "You are a grammar checker. Check the grammar of the story: {current_story}. Output only the suggested corrections as a list, or output 'Grammar is good!' if there are no errors.",
    OutputKey:   "grammar_suggestions",
})
if err != nil {
    log.Fatalf("Failed to create GrammarCheck agent: %v", err)
}

toneCheck, err := llmagent.New(llmagent.Config{
    Name:        "ToneCheck",
    Model:       model,
    Description: "Analyzes the tone of the story.",
    Instruction: "You are a tone analyzer. Analyze the tone of the story: {current_story}. Output only one word: 'positive' if the tone is generally positive, 'negative' if the tone is generally negative, or 'neutral' otherwise.",
    OutputKey:   "tone_check_result",
})
if err != nil {
    log.Fatalf("Failed to create ToneCheck agent: %v", err)
}

Part 4: Instantiating and Running the custom agent¶
Finally, you instantiate your StoryFlowAgent and use the Runner as usual.


Python
Go
Java

    // Instantiate the custom agent, which encapsulates the workflow agents.
    storyFlowAgent, err := NewStoryFlowAgent(
        storyGenerator,
        critic,
        reviser,
        grammarCheck,
        toneCheck,
    )
    if err != nil {
        log.Fatalf("Failed to create story flow agent: %v", err)
    }

    // --- Run the Agent ---
    sessionService := session.InMemoryService()
    initialState := map[string]any{
        "topic": "a brave kitten exploring a haunted house",
    }
    sessionInstance, err := sessionService.Create(ctx, &session.CreateRequest{
        AppName: appName,
        UserID:  userID,
        State:   initialState,
    })
    if err != nil {
        log.Fatalf("Failed to create session: %v", err)
    }

    userTopic := "a lonely robot finding a friend in a junkyard"

    r, err := runner.New(runner.Config{
        AppName:        appName,
        Agent:          storyFlowAgent,
        SessionService: sessionService,
    })
    if err != nil {
        log.Fatalf("Failed to create runner: %v", err)
    }

    input := genai.NewContentFromText("Generate a story about: "+userTopic, genai.RoleUser)
    events := r.Run(ctx, userID, sessionInstance.Session.ID(), input, agent.RunConfig{
        StreamingMode: agent.StreamingModeSSE,
    })

    var finalResponse string
    for event, err := range events {
        if err != nil {
            log.Fatalf("An error occurred during agent execution: %v", err)
        }

        for _, part := range event.Content.Parts {
            // Accumulate text from all parts of the final response.
            finalResponse += part.Text
        }
    }

    fmt.Println("\n--- Agent Interaction Result ---")
    fmt.Println("Agent Final Response: " + finalResponse)

    finalSession, err := sessionService.Get(ctx, &session.GetRequest{
        UserID:    userID,
        AppName:   appName,
        SessionID: sessionInstance.Session.ID(),
    })

    if err != nil {
        log.Fatalf("Failed to retrieve final session: %v", err)
    }

    fmt.Println("Final Session State:", finalSession.Session.State())
}

(Note: The full runnable code, including imports and execution logic, can be found linked below.)

Full Code Example¶
Storyflow Agent

Python
Go
Java

# Full runnable code for the StoryFlowAgent example
package main

import (
    "context"
    "fmt"
    "iter"
    "log"

    "google.golang.org/adk/agent/workflowagents/loopagent"
    "google.golang.org/adk/agent/workflowagents/sequentialagent"

    "google.golang.org/adk/agent"
    "google.golang.org/adk/agent/llmagent"
    "google.golang.org/adk/model/gemini"
    "google.golang.org/adk/runner"
    "google.golang.org/adk/session"
    "google.golang.org/genai"
)

// StoryFlowAgent is a custom agent that orchestrates a story generation workflow.
// It encapsulates the logic of running sub-agents in a specific sequence.
type StoryFlowAgent struct {
    storyGenerator     agent.Agent
    revisionLoopAgent  agent.Agent
    postProcessorAgent agent.Agent
}

// NewStoryFlowAgent creates and configures the entire custom agent workflow.
// It takes individual LLM agents as input and internally creates the necessary
// workflow agents (loop, sequential), returning the final orchestrator agent.
func NewStoryFlowAgent(
    storyGenerator,
    critic,
    reviser,
    grammarCheck,
    toneCheck agent.Agent,
) (agent.Agent, error) {
    loopAgent, err := loopagent.New(loopagent.Config{
        MaxIterations: 2,
        AgentConfig: agent.Config{
            Name:      "CriticReviserLoop",
            SubAgents: []agent.Agent{critic, reviser},
        },
    })
    if err != nil {
        return nil, fmt.Errorf("failed to create loop agent: %w", err)
    }

    sequentialAgent, err := sequentialagent.New(sequentialagent.Config{
        AgentConfig: agent.Config{
            Name:      "PostProcessing",
            SubAgents: []agent.Agent{grammarCheck, toneCheck},
        },
    })
    if err != nil {
        return nil, fmt.Errorf("failed to create sequential agent: %w", err)
    }

    // The StoryFlowAgent struct holds the agents needed for the Run method.
    orchestrator := &StoryFlowAgent{
        storyGenerator:     storyGenerator,
        revisionLoopAgent:  loopAgent,
        postProcessorAgent: sequentialAgent,
    }

    // agent.New creates the final agent, wiring up the Run method.
    return agent.New(agent.Config{
        Name:        "StoryFlowAgent",
        Description: "Orchestrates story generation, critique, revision, and checks.",
        SubAgents:   []agent.Agent{storyGenerator, loopAgent, sequentialAgent},
        Run:         orchestrator.Run,
    })
}


// Run defines the custom execution logic for the StoryFlowAgent.
func (s *StoryFlowAgent) Run(ctx agent.InvocationContext) iter.Seq2[*session.Event, error] {
    return func(yield func(*session.Event, error) bool) {
        // Stage 1: Initial Story Generation
        for event, err := range s.storyGenerator.Run(ctx) {
            if err != nil {
                yield(nil, fmt.Errorf("story generator failed: %w", err))
                return
            }
            if !yield(event, nil) {
                return
            }
        }

        // Check if story was generated before proceeding
        currentStory, err := ctx.Session().State().Get("current_story")
        if err != nil || currentStory == "" {
            log.Println("Failed to generate initial story. Aborting workflow.")
            return
        }

        // Stage 2: Critic-Reviser Loop
        for event, err := range s.revisionLoopAgent.Run(ctx) {
            if err != nil {
                yield(nil, fmt.Errorf("loop agent failed: %w", err))
                return
            }
            if !yield(event, nil) {
                return
            }
        }

        // Stage 3: Post-Processing
        for event, err := range s.postProcessorAgent.Run(ctx) {
            if err != nil {
                yield(nil, fmt.Errorf("sequential agent failed: %w", err))
                return
            }
            if !yield(event, nil) {
                return
            }
        }

        // Stage 4: Conditional Regeneration
        toneResult, err := ctx.Session().State().Get("tone_check_result")
        if err != nil {
            log.Printf("Could not read tone_check_result from state: %v. Assuming tone is not negative.", err)
            return
        }

        if tone, ok := toneResult.(string); ok && tone == "negative" {
            log.Println("Tone is negative. Regenerating story...")
            for event, err := range s.storyGenerator.Run(ctx) {
                if err != nil {
                    yield(nil, fmt.Errorf("story regeneration failed: %w", err))
                    return
                }
                if !yield(event, nil) {
                    return
                }
            }
        } else {
            log.Println("Tone is not negative. Keeping current story.")
        }
    }
}


const (
    modelName = "gemini-2.0-flash"
    appName   = "story_app"
    userID    = "user_12345"
)

func main() {
    ctx := context.Background()
    model, err := gemini.NewModel(ctx, modelName, &genai.ClientConfig{})
    if err != nil {
        log.Fatalf("Failed to create model: %v", err)
    }

    // --- Define the individual LLM agents ---
    storyGenerator, err := llmagent.New(llmagent.Config{
        Name:        "StoryGenerator",
        Model:       model,
        Description: "Generates the initial story.",
        Instruction: "You are a story writer. Write a short story (around 100 words) about a cat, based on the topic: {topic}",
        OutputKey:   "current_story",
    })
    if err != nil {
        log.Fatalf("Failed to create StoryGenerator agent: %v", err)
    }

    critic, err := llmagent.New(llmagent.Config{
        Name:        "Critic",
        Model:       model,
        Description: "Critiques the story.",
        Instruction: "You are a story critic. Review the story: {current_story}. Provide 1-2 sentences of constructive criticism on how to improve it. Focus on plot or character.",
        OutputKey:   "criticism",
    })
    if err != nil {
        log.Fatalf("Failed to create Critic agent: %v", err)
    }

    reviser, err := llmagent.New(llmagent.Config{
        Name:        "Reviser",
        Model:       model,
        Description: "Revises the story based on criticism.",
        Instruction: "You are a story reviser. Revise the story: {current_story}, based on the criticism: {criticism}. Output only the revised story.",
        OutputKey:   "current_story",
    })
    if err != nil {
        log.Fatalf("Failed to create Reviser agent: %v", err)
    }

    grammarCheck, err := llmagent.New(llmagent.Config{
        Name:        "GrammarCheck",
        Model:       model,
        Description: "Checks grammar and suggests corrections.",
        Instruction: "You are a grammar checker. Check the grammar of the story: {current_story}. Output only the suggested corrections as a list, or output 'Grammar is good!' if there are no errors.",
        OutputKey:   "grammar_suggestions",
    })
    if err != nil {
        log.Fatalf("Failed to create GrammarCheck agent: %v", err)
    }

    toneCheck, err := llmagent.New(llmagent.Config{
        Name:        "ToneCheck",
        Model:       model,
        Description: "Analyzes the tone of the story.",
        Instruction: "You are a tone analyzer. Analyze the tone of the story: {current_story}. Output only one word: 'positive' if the tone is generally positive, 'negative' if the tone is generally negative, or 'neutral' otherwise.",
        OutputKey:   "tone_check_result",
    })
    if err != nil {
        log.Fatalf("Failed to create ToneCheck agent: %v", err)
    }

    // Instantiate the custom agent, which encapsulates the workflow agents.
    storyFlowAgent, err := NewStoryFlowAgent(
        storyGenerator,
        critic,
        reviser,
        grammarCheck,
        toneCheck,
    )
    if err != nil {
        log.Fatalf("Failed to create story flow agent: %v", err)
    }

    // --- Run the Agent ---
    sessionService := session.InMemoryService()
    initialState := map[string]any{
        "topic": "a brave kitten exploring a haunted house",
    }
    sessionInstance, err := sessionService.Create(ctx, &session.CreateRequest{
        AppName: appName,
        UserID:  userID,
        State:   initialState,
    })
    if err != nil {
        log.Fatalf("Failed to create session: %v", err)
    }

    userTopic := "a lonely robot finding a friend in a junkyard"

    r, err := runner.New(runner.Config{
        AppName:        appName,
        Agent:          storyFlowAgent,
        SessionService: sessionService,
    })
    if err != nil {
        log.Fatalf("Failed to create runner: %v", err)
    }

    input := genai.NewContentFromText("Generate a story about: "+userTopic, genai.RoleUser)
    events := r.Run(ctx, userID, sessionInstance.Session.ID(), input, agent.RunConfig{
        StreamingMode: agent.StreamingModeSSE,
    })

    var finalResponse string
    for event, err := range events {
        if err != nil {
            log.Fatalf("An error occurred during agent execution: %v", err)
        }

        for _, part := range event.Content.Parts {
            // Accumulate text from all parts of the final response.
            finalResponse += part.Text
        }
    }

    fmt.Println("\n--- Agent Interaction Result ---")
    fmt.Println("Agent Final Response: " + finalResponse)

    finalSession, err := sessionService.Get(ctx, &session.GetRequest{
        UserID:    userID,
        AppName:   appName,
        SessionID: sessionInstance.Session.ID(),
    })

    if err != nil {
        log.Fatalf("Failed to retrieve final session: %v", err)
    }

    fmt.Println("Final Session State:", finalSession.Session.State())
}