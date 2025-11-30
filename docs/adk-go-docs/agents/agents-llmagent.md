LLM Agent¶
Supported in ADKPython v0.1.0Go v0.1.0Java v0.1.0
The LlmAgent (often aliased simply as Agent) is a core component in ADK, acting as the "thinking" part of your application. It leverages the power of a Large Language Model (LLM) for reasoning, understanding natural language, making decisions, generating responses, and interacting with tools.

Unlike deterministic Workflow Agents that follow predefined execution paths, LlmAgent behavior is non-deterministic. It uses the LLM to interpret instructions and context, deciding dynamically how to proceed, which tools to use (if any), or whether to transfer control to another agent.

Building an effective LlmAgent involves defining its identity, clearly guiding its behavior through instructions, and equipping it with the necessary tools and capabilities.

Defining the Agent's Identity and Purpose¶
First, you need to establish what the agent is and what it's for.

name (Required): Every agent needs a unique string identifier. This name is crucial for internal operations, especially in multi-agent systems where agents need to refer to or delegate tasks to each other. Choose a descriptive name that reflects the agent's function (e.g., customer_support_router, billing_inquiry_agent). Avoid reserved names like user.

description (Optional, Recommended for Multi-Agent): Provide a concise summary of the agent's capabilities. This description is primarily used by other LLM agents to determine if they should route a task to this agent. Make it specific enough to differentiate it from peers (e.g., "Handles inquiries about current billing statements," not just "Billing agent").

model (Required): Specify the underlying LLM that will power this agent's reasoning. This is a string identifier like "gemini-2.0-flash". The choice of model impacts the agent's capabilities, cost, and performance. See the Models page for available options and considerations.


Python
Go
Java

// Example: Defining the basic identity
agent, err := llmagent.New(llmagent.Config{
    Name:        "capital_agent",
    Model:       model,
    Description: "Answers user questions about the capital city of a given country.",
    // instruction and tools will be added next
})

Guiding the Agent: Instructions (instruction)¶
The instruction parameter is arguably the most critical for shaping an LlmAgent's behavior. It's a string (or a function returning a string) that tells the agent:

Its core task or goal.
Its personality or persona (e.g., "You are a helpful assistant," "You are a witty pirate").
Constraints on its behavior (e.g., "Only answer questions about X," "Never reveal Y").
How and when to use its tools. You should explain the purpose of each tool and the circumstances under which it should be called, supplementing any descriptions within the tool itself.
The desired format for its output (e.g., "Respond in JSON," "Provide a bulleted list").
Tips for Effective Instructions:

Be Clear and Specific: Avoid ambiguity. Clearly state the desired actions and outcomes.
Use Markdown: Improve readability for complex instructions using headings, lists, etc.
Provide Examples (Few-Shot): For complex tasks or specific output formats, include examples directly in the instruction.
Guide Tool Use: Don't just list tools; explain when and why the agent should use them.
State:

The instruction is a string template, you can use the {var} syntax to insert dynamic values into the instruction.
{var} is used to insert the value of the state variable named var.
{artifact.var} is used to insert the text content of the artifact named var.
If the state variable or artifact does not exist, the agent will raise an error. If you want to ignore the error, you can append a ? to the variable name as in {var?}.

Python
Go
Java

    // Example: Adding instructions
    agent, err := llmagent.New(llmagent.Config{
        Name:        "capital_agent",
        Model:       model,
        Description: "Answers user questions about the capital city of a given country.",
        Instruction: `You are an agent that provides the capital city of a country.
When a user asks for the capital of a country:
1. Identify the country name from the user's query.
2. Use the 'get_capital_city' tool to find the capital.
3. Respond clearly to the user, stating the capital city.
Example Query: "What's the capital of {country}?"
Example Response: "The capital of France is Paris."`,
        // tools will be added next
    })

(Note: For instructions that apply to all agents in a system, consider using global_instruction on the root agent, detailed further in the Multi-Agents section.)

Equipping the Agent: Tools (tools)¶
Tools give your LlmAgent capabilities beyond the LLM's built-in knowledge or reasoning. They allow the agent to interact with the outside world, perform calculations, fetch real-time data, or execute specific actions.

tools (Optional): Provide a list of tools the agent can use. Each item in the list can be:
A native function or method (wrapped as a FunctionTool). Python ADK automatically wraps the native function into a FuntionTool whereas, you must explicitly wrap your Java methods using FunctionTool.create(...)
An instance of a class inheriting from BaseTool.
An instance of another agent (AgentTool, enabling agent-to-agent delegation - see Multi-Agents).
The LLM uses the function/tool names, descriptions (from docstrings or the description field), and parameter schemas to decide which tool to call based on the conversation and its instructions.


Python
Go
Java

// Define a tool function
type getCapitalCityArgs struct {
    Country string `json:"country" jsonschema:"The country to get the capital of."`
}
getCapitalCity := func(ctx tool.Context, args getCapitalCityArgs) (map[string]any, error) {
    // Replace with actual logic (e.g., API call, database lookup)
    capitals := map[string]string{"france": "Paris", "japan": "Tokyo", "canada": "Ottawa"}
    capital, ok := capitals[strings.ToLower(args.Country)]
    if !ok {
        return nil, fmt.Errorf("Sorry, I don't know the capital of %s.", args.Country)
    }
    return map[string]any{"result": capital}, nil
}

// Add the tool to the agent
capitalTool, err := functiontool.New(
    functiontool.Config{
        Name:        "get_capital_city",
        Description: "Retrieves the capital city for a given country.",
    },
    getCapitalCity,
)
if err != nil {
    log.Fatal(err)
}
agent, err := llmagent.New(llmagent.Config{
    Name:        "capital_agent",
    Model:       model,
    Description: "Answers user questions about the capital city of a given country.",
    Instruction: "You are an agent that provides the capital city of a country... (previous instruction text)",
    Tools:       []tool.Tool{capitalTool},
})

Learn more about Tools in the Tools section.

Advanced Configuration & Control¶
Beyond the core parameters, LlmAgent offers several options for finer control:

Configuring LLM Generation (generate_content_config)¶
You can adjust how the underlying LLM generates responses using generate_content_config.

generate_content_config (Optional): Pass an instance of google.genai.types.GenerateContentConfig to control parameters like temperature (randomness), max_output_tokens (response length), top_p, top_k, and safety settings.

Python
Go
Java

import "google.golang.org/genai"

temperature := float32(0.2)
agent, err := llmagent.New(llmagent.Config{
    Name:  "gen_config_agent",
    Model: model,
    GenerateContentConfig: &genai.GenerateContentConfig{
        Temperature:     &temperature,
        MaxOutputTokens: 250,
    },
})

Structuring Data (input_schema, output_schema, output_key)¶
For scenarios requiring structured data exchange with an LLM Agent, the ADK provides mechanisms to define expected input and desired output formats using schema definitions.

input_schema (Optional): Define a schema representing the expected input structure. If set, the user message content passed to this agent must be a JSON string conforming to this schema. Your instructions should guide the user or preceding agent accordingly.

output_schema (Optional): Define a schema representing the desired output structure. If set, the agent's final response must be a JSON string conforming to this schema.

output_key (Optional): Provide a string key. If set, the text content of the agent's final response will be automatically saved to the session's state dictionary under this key. This is useful for passing results between agents or steps in a workflow.

In Python, this might look like: session.state[output_key] = agent_response_text
In Java: session.state().put(outputKey, agentResponseText)
In Golang, within a callback handler: ctx.State().Set(output_key, agentResponseText)

Python
Go
Java
The input and output schema is a google.genai.types.Schema object.


capitalOutput := &genai.Schema{
    Type:        genai.TypeObject,
    Description: "Schema for capital city information.",
    Properties: map[string]*genai.Schema{
        "capital": {
            Type:        genai.TypeString,
            Description: "The capital city of the country.",
        },
    },
}

agent, err := llmagent.New(llmagent.Config{
    Name:         "structured_capital_agent",
    Model:        model,
    Description:  "Provides capital information in a structured format.",
    Instruction:  `You are a Capital Information Agent. Given a country, respond ONLY with a JSON object containing the capital. Format: {"capital": "capital_name"}`,
    OutputSchema: capitalOutput,
    OutputKey:    "found_capital",
    // Cannot use the capitalTool tool effectively here
})

Managing Context (include_contents)¶
Control whether the agent receives the prior conversation history.

include_contents (Optional, Default: 'default'): Determines if the contents (history) are sent to the LLM.
'default': The agent receives the relevant conversation history.
'none': The agent receives no prior contents. It operates based solely on its current instruction and any input provided in the current turn (useful for stateless tasks or enforcing specific contexts).

Python
Go
Java

import "google.golang.org/adk/agent/llmagent"

agent, err := llmagent.New(llmagent.Config{
    Name:            "stateless_agent",
    Model:           model,
    IncludeContents: llmagent.IncludeContentsNone,
})