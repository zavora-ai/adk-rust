Parallel agents¶
Supported in ADKPython v0.1.0Go v0.1.0Java v0.2.0
The ParallelAgent is a workflow agent that executes its sub-agents concurrently. This dramatically speeds up workflows where tasks can be performed independently.

Use ParallelAgent when: For scenarios prioritizing speed and involving independent, resource-intensive tasks, a ParallelAgent facilitates efficient parallel execution. When sub-agents operate without dependencies, their tasks can be performed concurrently, significantly reducing overall processing time.

As with other workflow agents, the ParallelAgent is not powered by an LLM, and is thus deterministic in how it executes. That being said, workflow agents are only concerned with their execution (i.e. executing sub-agents in parallel), and not their internal logic; the tools or sub-agents of a workflow agent may or may not utilize LLMs.

Example¶
This approach is particularly beneficial for operations like multi-source data retrieval or heavy computations, where parallelization yields substantial performance gains. Importantly, this strategy assumes no inherent need for shared state or direct information exchange between the concurrently executing agents.

How it works¶
When the ParallelAgent's run_async() method is called:

Concurrent Execution: It initiates the run_async() method of each sub-agent present in the sub_agents list concurrently. This means all the agents start running at (approximately) the same time.
Independent Branches: Each sub-agent operates in its own execution branch. There is no automatic sharing of conversation history or state between these branches during execution.
Result Collection: The ParallelAgent manages the parallel execution and, typically, provides a way to access the results from each sub-agent after they have completed (e.g., through a list of results or events). The order of results may not be deterministic.
Independent Execution and State Management¶
It's crucial to understand that sub-agents within a ParallelAgent run independently. If you need communication or data sharing between these agents, you must implement it explicitly. Possible approaches include:

Shared InvocationContext: You could pass a shared InvocationContext object to each sub-agent. This object could act as a shared data store. However, you'd need to manage concurrent access to this shared context carefully (e.g., using locks) to avoid race conditions.
External State Management: Use an external database, message queue, or other mechanism to manage shared state and facilitate communication between agents.
Post-Processing: Collect results from each branch, and then implement logic to coordinate data afterwards.
Parallel Agent

Full Example: Parallel Web Research¶
Imagine researching multiple topics simultaneously:

Researcher Agent 1: An LlmAgent that researches "renewable energy sources."
Researcher Agent 2: An LlmAgent that researches "electric vehicle technology."
Researcher Agent 3: An LlmAgent that researches "carbon capture methods."


ParallelAgent(sub_agents=[ResearcherAgent1, ResearcherAgent2, ResearcherAgent3])
These research tasks are independent. Using a ParallelAgent allows them to run concurrently, potentially reducing the total research time significantly compared to running them sequentially. The results from each agent would be collected separately after they finish.

Full Code

Python
Go
Java

    model, err := gemini.NewModel(ctx, modelName, &genai.ClientConfig{})
    if err != nil {
        return fmt.Errorf("failed to create model: %v", err)
    }

    // --- 1. Define Researcher Sub-Agents (to run in parallel) ---
    researcher1, err := llmagent.New(llmagent.Config{
        Name:  "RenewableEnergyResearcher",
        Model: model,
        Instruction: `You are an AI Research Assistant specializing in energy.
 Research the latest advancements in 'renewable energy sources'.
 Use the Google Search tool provided.
 Summarize your key findings concisely (1-2 sentences).
 Output *only* the summary.`,
        Description: "Researches renewable energy sources.",
        OutputKey:   "renewable_energy_result",
    })
    if err != nil {
        return err
    }
    researcher2, err := llmagent.New(llmagent.Config{
        Name:  "EVResearcher",
        Model: model,
        Instruction: `You are an AI Research Assistant specializing in transportation.
 Research the latest developments in 'electric vehicle technology'.
 Use the Google Search tool provided.
 Summarize your key findings concisely (1-2 sentences).
 Output *only* the summary.`,
        Description: "Researches electric vehicle technology.",
        OutputKey:   "ev_technology_result",
    })
    if err != nil {
        return err
    }
    researcher3, err := llmagent.New(llmagent.Config{
        Name:  "CarbonCaptureResearcher",
        Model: model,
        Instruction: `You are an AI Research Assistant specializing in climate solutions.
 Research the current state of 'carbon capture methods'.
 Use the Google Search tool provided.
 Summarize your key findings concisely (1-2 sentences).
 Output *only* the summary.`,
        Description: "Researches carbon capture methods.",
        OutputKey:   "carbon_capture_result",
    })
    if err != nil {
        return err
    }

    // --- 2. Create the ParallelAgent (Runs researchers concurrently) ---
    parallelResearchAgent, err := parallelagent.New(parallelagent.Config{
        AgentConfig: agent.Config{
            Name:        "ParallelWebResearchAgent",
            Description: "Runs multiple research agents in parallel to gather information.",
            SubAgents:   []agent.Agent{researcher1, researcher2, researcher3},
        },
    })
    if err != nil {
        return fmt.Errorf("failed to create parallel agent: %v", err)
    }

    // --- 3. Define the Merger Agent (Runs *after* the parallel agents) ---
    synthesisAgent, err := llmagent.New(llmagent.Config{
        Name:  "SynthesisAgent",
        Model: model,
        Instruction: `You are an AI Assistant responsible for combining research findings into a structured report.
 Your primary task is to synthesize the following research summaries, clearly attributing findings to their source areas. Structure your response using headings for each topic. Ensure the report is coherent and integrates the key points smoothly.
 **Crucially: Your entire response MUST be grounded *exclusively* on the information provided in the 'Input Summaries' below. Do NOT add any external knowledge, facts, or details not present in these specific summaries.**
 **Input Summaries:**

 *   **Renewable Energy:**
     {renewable_energy_result}

 *   **Electric Vehicles:**
     {ev_technology_result}

 *   **Carbon Capture:**
     {carbon_capture_result}

 **Output Format:**

 ## Summary of Recent Sustainable Technology Advancements

 ### Renewable Energy Findings
 (Based on RenewableEnergyResearcher's findings)
 [Synthesize and elaborate *only* on the renewable energy input summary provided above.]

 ### Electric Vehicle Findings
 (Based on EVResearcher's findings)
 [Synthesize and elaborate *only* on the EV input summary provided above.]

 ### Carbon Capture Findings
 (Based on CarbonCaptureResearcher's findings)
 [Synthesize and elaborate *only* on the carbon capture input summary provided above.]

 ### Overall Conclusion
 [Provide a brief (1-2 sentence) concluding statement that connects *only* the findings presented above.]

 Output *only* the structured report following this format. Do not include introductory or concluding phrases outside this structure, and strictly adhere to using only the provided input summary content.`,
        Description: "Combines research findings from parallel agents into a structured, cited report, strictly grounded on provided inputs.",
    })
    if err != nil {
        return fmt.Errorf("failed to create synthesis agent: %v", err)
    }

    // --- 4. Create the SequentialAgent (Orchestrates the overall flow) ---
    pipeline, err := sequentialagent.New(sequentialagent.Config{
        AgentConfig: agent.Config{
            Name:        "ResearchAndSynthesisPipeline",
            Description: "Coordinates parallel research and synthesizes the results.",
            SubAgents:   []agent.Agent{parallelResearchAgent, synthesisAgent},
        },
    })
    if err != nil {
        return fmt.Errorf("failed to create sequential agent pipeline: %v", err)