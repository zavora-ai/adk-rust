README ¶
Agent Development Kit (ADK) for Go
License Go Doc Nightly Check r/agentdevelopmentkit Ask DeepWiki


An open-source, code-first Go toolkit for building, evaluating, and deploying sophisticated AI agents with flexibility and control.
Important Links: Docs & Samples & Python ADK & Java ADK & ADK Web.
Agent Development Kit (ADK) is a flexible and modular framework that applies software development principles to AI agent creation. It is designed to simplify building, deploying, and orchestrating agent workflows, from simple tasks to complex systems. While optimized for Gemini, ADK is model-agnostic, deployment-agnostic, and compatible with other frameworks.

This Go version of ADK is ideal for developers building cloud-native agent applications, leveraging Go's strengths in concurrency and performance.

✨ Key Features
Idiomatic Go: Designed to feel natural and leverage the power of Go.
Rich Tool Ecosystem: Utilize pre-built tools, custom functions, or integrate existing tools to give agents diverse capabilities.
Code-First Development: Define agent logic, tools, and orchestration directly in Go for ultimate flexibility, testability, and versioning.
Modular Multi-Agent Systems: Design scalable applications by composing multiple specialized agents.
Deploy Anywhere: Easily containerize and deploy agents, with strong support for cloud-native environments like Google Cloud Run.
🚀 Installation
To add ADK Go to your project, run:

go get google.golang.org/adk
📄 License
This project is licensed under the Apache 2.0 License - see the LICENSE file for details.

The exception is internal/httprr - see its LICENSE file.

Expand ▾
 Directories ¶
Show internal
Expand all
agent
Package agent provides entities to build agents using ADK.
llmagent
Package llmagent provides a way to build LLM-based agents.
remoteagent
Package remoteagent allows to use a remote agent via A2A protocol.
workflowagents/loopagent
Package loopagent provides an agent that repeatedly runs its sub-agents for a specified number of iterations or until termination condition is met.
workflowagents/parallelagent
Package parallelagent provides an agent that runs its sub-agents in parallel.
workflowagents/sequentialagent
Package sequentialagent provides an agent that runs its sub-agents in a sequence.
artifact
Package artifact provides a service for managing artifacts.
gcsartifact
Package gcs provides a Google Cloud Storage (GCS) implementation of the artifact.Service interface.
cmd
adkgo command
Adkgo is a CLI tool to help deploy and test an ADK application.
launcher
Package launcher provides ways to interact with agents
launcher/console
Package console provides a simple way to interact with an agent from console application
launcher/full
Package full provides easy way to play with ADK with all available options
launcher/prod
Package prod provides easy way to play with ADK with all available options without development support (no console, no ADK Web UI, just API and A2A )
launcher/universal
Package universal provides an umbrella over launchers (console and web).
launcher/web
Package web provides a way to run ADK using a web server (extended by sublaunchers)
launcher/web/a2a
Package a2a provides a sublauncher that adds A2A capabilities to the web server
launcher/web/api
Package api provides a sublauncher that adds ADK REST API to the web server (using url /api/)
launcher/web/webui
Package webui provides a sublauncher that adds ADK Web UI to the web server (using url /ui/)
examples
a2a command
mcp command
quickstart command
rest command
This example demonstrates how to use the ADK REST API handler directly with the standard net/http package, without relying on any specific router.
tools/loadartifacts command
tools/multipletools command
vertexai/imagegenerator command
Package main demonstrates how to create an agent that can generate images using Vertex AI's Imagen model, save them as artifacts, and then save them to the local filesystem.
web command
web/agents
Package agents contains sample agents to demonstate ADK Web Capabilities.
workflowagents/loop command
workflowagents/parallel command
workflowagents/sequential command
workflowagents/sequentialCode command
memory
Package memory defines the entities to interact with agent memory (long-term knowledge).
model
Package model defines the interfaces and data structures for interacting with LLMs.
gemini
Package gemini implements the model.LLM interface for Gemini models.
runner
Package runner provides a runtime for ADK agents.
server
Package server hosts protocol implementations to expose and serve ADK agents.
adka2a
Package adka2a allows to expose ADK agents via A2A.
adkrest
adkrest/controllers
Package controllers contains the controllers for the ADK-Web REST API.
session
Package session provides types to manage user sessions and their states.
database
telemetry
Package telemetry allows to set up custom telemetry processors that the ADK events will be emitted to.
tool
Package tool defines the interfaces for tools that can be called by an agent.
agenttool
Package agenttool provides a tool that allows an agent to call another agent.
exitlooptool
Package exitlooptool provides a tool that allows an agent to exit a loop.
functiontool
Package functiontool provides a tool that wraps a Go function.
geminitool
Package geminitool provides access to Gemini native tools.
loadartifactstool
Package loadartifactstool defines a tool for loading artifacts.
mcptoolset
mcptool package provides MCP adapter, allowing to add MCP tools to LLMAgent.
util
instructionutil
