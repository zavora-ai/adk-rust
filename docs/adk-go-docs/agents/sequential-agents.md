Sequential agents¶
Supported in ADKPython v0.1.0Go v0.1.0Java v0.2.0
The SequentialAgent is a workflow agent that executes its sub-agents in the order they are specified in the list. Use the SequentialAgent when you want the execution to occur in a fixed, strict order.

Example¶
You want to build an agent that can summarize any webpage, using two tools: Get Page Contents and Summarize Page. Because the agent must always call Get Page Contents before calling Summarize Page (you can't summarize from nothing!), you should build your agent using a SequentialAgent.
As with other workflow agents, the SequentialAgent is not powered by an LLM, and is thus deterministic in how it executes. That being said, workflow agents are concerned only with their execution (i.e. in sequence), and not their internal logic; the tools or sub-agents of a workflow agent may or may not utilize LLMs.

How it works¶
When the SequentialAgent's Run Async method is called, it performs the following actions:

Iteration: It iterates through the sub agents list in the order they were provided.
Sub-Agent Execution: For each sub-agent in the list, it calls the sub-agent's Run Async method.
Sequential Agent

Full Example: Code Development Pipeline¶
Consider a simplified code development pipeline:

Code Writer Agent: An LLM Agent that generates initial code based on a specification.
Code Reviewer Agent: An LLM Agent that reviews the generated code for errors, style issues, and adherence to best practices. It receives the output of the Code Writer Agent.
Code Refactorer Agent: An LLM Agent that takes the reviewed code (and the reviewer's comments) and refactors it to improve quality and address issues.
A SequentialAgent is perfect for this:


SequentialAgent(sub_agents=[CodeWriterAgent, CodeReviewerAgent, CodeRefactorerAgent])
This ensures the code is written, then reviewed, and finally refactored, in a strict, dependable order. The output from each sub-agent is passed to the next by storing them in state via Output Key.

Shared Invocation Context

The SequentialAgent passes the same InvocationContext to each of its sub-agents. This means they all share the same session state, including the temporary (temp:) namespace, making it easy to pass data between steps within a single turn.

Code

Python
Go
Java

    model, err := gemini.NewModel(ctx, modelName, &genai.ClientConfig{})
    if err != nil {
        return fmt.Errorf("failed to create model: %v", err)
    }

    codeWriterAgent, err := llmagent.New(llmagent.Config{
        Name:        "CodeWriterAgent",
        Model:       model,
        Description: "Writes initial Go code based on a specification.",
        Instruction: `You are a Go Code Generator.
Based *only* on the user's request, write Go code that fulfills the requirement.
Output *only* the complete Go code block, enclosed in triple backticks ('''go ... ''').
Do not add any other text before or after the code block.`,
        OutputKey: "generated_code",
    })
    if err != nil {
        return fmt.Errorf("failed to create code writer agent: %v", err)
    }

    codeReviewerAgent, err := llmagent.New(llmagent.Config{
        Name:        "CodeReviewerAgent",
        Model:       model,
        Description: "Reviews code and provides feedback.",
        Instruction: `You are an expert Go Code Reviewer.
Your task is to provide constructive feedback on the provided code.

**Code to Review:**
'''go
{generated_code}
'''

**Review Criteria:**
1.  **Correctness:** Does the code work as intended? Are there logic errors?
2.  **Readability:** Is the code clear and easy to understand? Follows Go style guidelines?
3.  **Idiomatic Go:** Does the code use Go's features in a natural and standard way?
4.  **Edge Cases:** Does the code handle potential edge cases or invalid inputs gracefully?
5.  **Best Practices:** Does the code follow common Go best practices?

**Output:**
Provide your feedback as a concise, bulleted list. Focus on the most important points for improvement.
If the code is excellent and requires no changes, simply state: "No major issues found."
Output *only* the review comments or the "No major issues" statement.`,
        OutputKey: "review_comments",
    })
    if err != nil {
        return fmt.Errorf("failed to create code reviewer agent: %v", err)
    }

    codeRefactorerAgent, err := llmagent.New(llmagent.Config{
        Name:        "CodeRefactorerAgent",
        Model:       model,
        Description: "Refactors code based on review comments.",
        Instruction: `You are a Go Code Refactoring AI.
Your goal is to improve the given Go code based on the provided review comments.

**Original Code:**
'''go
{generated_code}
'''

**Review Comments:**
{review_comments}

**Task:**
Carefully apply the suggestions from the review comments to refactor the original code.
If the review comments state "No major issues found," return the original code unchanged.
Ensure the final code is complete, functional, and includes necessary imports.

**Output:**
Output *only* the final, refactored Go code block, enclosed in triple backticks ('''go ... ''').
Do not add any other text before or after the code block.`,
        OutputKey: "refactored_code",
    })
    if err != nil {
        return fmt.Errorf("failed to create code refactorer agent: %v", err)
    }

    codePipelineAgent, err := sequentialagent.New(sequentialagent.Config{
        AgentConfig: agent.Config{
            Name:        appName,
            Description: "Executes a sequence of code writing, reviewing, and refactoring.",
            SubAgents: []agent.Agent{
                codeWriterAgent,
                codeReviewerAgent,
                codeRefactorerAgent,
            },
        },
    })
    if err != nil {
        return fmt.Errorf("failed to create sequential agent: %v", err)
    }