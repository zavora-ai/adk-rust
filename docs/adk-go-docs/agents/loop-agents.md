Loop agents¶
Supported in ADKPython v0.1.0Go v0.1.0Java v0.2.0
The LoopAgent is a workflow agent that executes its sub-agents in a loop (i.e. iteratively). It repeatedly runs a sequence of agents for a specified number of iterations or until a termination condition is met.

Use the LoopAgent when your workflow involves repetition or iterative refinement, such as revising code.

Example¶
You want to build an agent that can generate images of food, but sometimes when you want to generate a specific number of items (e.g. 5 bananas), it generates a different number of those items in the image (e.g. an image of 7 bananas). You have two tools: Generate Image, Count Food Items. Because you want to keep generating images until it either correctly generates the specified number of items, or after a certain number of iterations, you should build your agent using a LoopAgent.
As with other workflow agents, the LoopAgent is not powered by an LLM, and is thus deterministic in how it executes. That being said, workflow agents are only concerned only with their execution (i.e. in a loop), and not their internal logic; the tools or sub-agents of a workflow agent may or may not utilize LLMs.

How it Works¶
When the LoopAgent's Run Async method is called, it performs the following actions:

Sub-Agent Execution: It iterates through the Sub Agents list in order. For each sub-agent, it calls the agent's Run Async method.
Termination Check:

Crucially, the LoopAgent itself does not inherently decide when to stop looping. You must implement a termination mechanism to prevent infinite loops. Common strategies include:

Max Iterations: Set a maximum number of iterations in the LoopAgent. The loop will terminate after that many iterations.
Escalation from sub-agent: Design one or more sub-agents to evaluate a condition (e.g., "Is the document quality good enough?", "Has a consensus been reached?"). If the condition is met, the sub-agent can signal termination (e.g., by raising a custom event, setting a flag in a shared context, or returning a specific value).
Loop Agent

Full Example: Iterative Document Improvement¶
Imagine a scenario where you want to iteratively improve a document:

Writer Agent: An LlmAgent that generates or refines a draft on a topic.
Critic Agent: An LlmAgent that critiques the draft, identifying areas for improvement.


LoopAgent(sub_agents=[WriterAgent, CriticAgent], max_iterations=5)
In this setup, the LoopAgent would manage the iterative process. The CriticAgent could be designed to return a "STOP" signal when the document reaches a satisfactory quality level, preventing further iterations. Alternatively, the max iterations parameter could be used to limit the process to a fixed number of cycles, or external logic could be implemented to make stop decisions. The loop would run at most five times, ensuring the iterative refinement doesn't continue indefinitely.

Full Code

Python
Go
Java

// ExitLoopArgs defines the (empty) arguments for the ExitLoop tool.
type ExitLoopArgs struct{}

// ExitLoopResults defines the output of the ExitLoop tool.
type ExitLoopResults struct{}

// ExitLoop is a tool that signals the loop to terminate by setting Escalate to true.
func ExitLoop(ctx tool.Context, input ExitLoopArgs) (ExitLoopResults, error) {
    fmt.Printf("[Tool Call] exitLoop triggered by %s \n", ctx.AgentName())
    ctx.Actions().Escalate = true
    return ExitLoopResults{}, nil
}

func main() {
    ctx := context.Background()

    if err := runAgent(ctx, "Write a document about a cat"); err != nil {
        log.Fatalf("Agent execution failed: %v", err)
    }
}

func runAgent(ctx context.Context, prompt string) error {
    model, err := gemini.NewModel(ctx, modelName, &genai.ClientConfig{})
    if err != nil {
        return fmt.Errorf("failed to create model: %v", err)
    }

    // STEP 1: Initial Writer Agent (Runs ONCE at the beginning)
    initialWriterAgent, err := llmagent.New(llmagent.Config{
        Name:        "InitialWriterAgent",
        Model:       model,
        Description: "Writes the initial document draft based on the topic.",
        Instruction: `You are a Creative Writing Assistant tasked with starting a story.
Write the *first draft* of a short story (aim for 2-4 sentences).
Base the content *only* on the topic provided in the user's prompt.
Output *only* the story/document text. Do not add introductions or explanations.`,
        OutputKey: stateDoc,
    })
    if err != nil {
        return fmt.Errorf("failed to create initial writer agent: %v", err)
    }

    // STEP 2a: Critic Agent (Inside the Refinement Loop)
    criticAgentInLoop, err := llmagent.New(llmagent.Config{
        Name:        "CriticAgent",
        Model:       model,
        Description: "Reviews the current draft, providing critique or signaling completion.",
        Instruction: fmt.Sprintf(`You are a Constructive Critic AI reviewing a short document draft.
**Document to Review:**
"""
{%s}
"""
**Task:**
Review the document.
IF you identify 1-2 *clear and actionable* ways it could be improved:
Provide these specific suggestions concisely. Output *only* the critique text.
ELSE IF the document is coherent and addresses the topic adequately:
Respond *exactly* with the phrase "%s" and nothing else.`, stateDoc, donePhrase),
        OutputKey: stateCrit,
    })
    if err != nil {
        return fmt.Errorf("failed to create critic agent: %v", err)
    }

    exitLoopTool, err := functiontool.New(
        functiontool.Config{
            Name:        "exitLoop",
            Description: "Call this function ONLY when the critique indicates no further changes are needed.",
        },
        ExitLoop,
    )
    if err != nil {
        return fmt.Errorf("failed to create exit loop tool: %v", err)
    }

    // STEP 2b: Refiner/Exiter Agent (Inside the Refinement Loop)
    refinerAgentInLoop, err := llmagent.New(llmagent.Config{
        Name:  "RefinerAgent",
        Model: model,
        Instruction: fmt.Sprintf(`You are a Creative Writing Assistant refining a document based on feedback OR exiting the process.
**Current Document:**

"""
{%s}
"""

**Critique/Suggestions:**
{%s}
**Task:**
Analyze the 'Critique/Suggestions'.
IF the critique is *exactly* "%s":
You MUST call the 'exitLoop' function. Do not output any text.
ELSE (the critique contains actionable feedback):
Carefully apply the suggestions to improve the 'Current Document'. Output *only* the refined document text.`, stateDoc, stateCrit, donePhrase),
        Description: "Refines the document based on critique, or calls exitLoop if critique indicates completion.",
        Tools:       []tool.Tool{exitLoopTool},
        OutputKey:   stateDoc,
    })
    if err != nil {
        return fmt.Errorf("failed to create refiner agent: %v", err)
    }

    // STEP 2: Refinement Loop Agent
    refinementLoop, err := loopagent.New(loopagent.Config{
        AgentConfig: agent.Config{
            Name:      "RefinementLoop",
            SubAgents: []agent.Agent{criticAgentInLoop, refinerAgentInLoop},
        },
        MaxIterations: 5,
    })
    if err != nil {
        return fmt.Errorf("failed to create loop agent: %v", err)
    }

    // STEP 3: Overall Sequential Pipeline
    iterativeWriterAgent, err := sequentialagent.New(sequentialagent.Config{
        AgentConfig: agent.Config{
            Name:      appName,
            SubAgents: []agent.Agent{initialWriterAgent, refinementLoop},
        },
    })
    if err != nil {
        return fmt.Errorf("failed to create sequential agent pipeline: %v", err)
    }