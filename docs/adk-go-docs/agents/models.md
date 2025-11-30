Using Different Models with ADK¶
Supported in ADKPython v0.1.0Go v0.1.0Java v0.1.0
The Agent Development Kit (ADK) is designed for flexibility, allowing you to integrate various Large Language Models (LLMs) into your agents. While the setup for Google Gemini models is covered in the Setup Foundation Models guide, this page details how to leverage Gemini effectively and integrate other popular models, including those hosted externally or running locally.

ADK primarily uses two mechanisms for model integration:

Direct String / Registry: For models tightly integrated with Google Cloud (like Gemini models accessed via Google AI Studio or Vertex AI) or models hosted on Vertex AI endpoints. You typically provide the model name or endpoint resource string directly to the LlmAgent. ADK's internal registry resolves this string to the appropriate backend client, often utilizing the google-genai library.
Wrapper Classes: For broader compatibility, especially with models outside the Google ecosystem or those requiring specific client configurations (like models accessed via Apigee or LiteLLM). You instantiate a specific wrapper class (e.g., ApigeeLlm or LiteLlm) and pass this object as the model parameter to your LlmAgent.
The following sections guide you through using these methods based on your needs.

Using Google Gemini Models¶
This section covers authenticating with Google's Gemini models, either through Google AI Studio for rapid development or Google Cloud Vertex AI for enterprise applications. This is the most direct way to use Google's flagship models within ADK.

Integration Method: Once you are authenticated using one of the below methods, you can pass the model's identifier string directly to the model parameter of LlmAgent.

Tip

The google-genai library, used internally by ADK for Gemini models, can connect through either Google AI Studio or Vertex AI.

Model support for voice/video streaming

In order to use voice/video streaming in ADK, you will need to use Gemini models that support the Live API. You can find the model ID(s) that support the Gemini Live API in the documentation:

Google AI Studio: Gemini Live API
Vertex AI: Gemini Live API
Google AI Studio¶
This is the simplest method and is recommended for getting started quickly.

Authentication Method: API Key
Setup:

Get an API key: Obtain your key from Google AI Studio.
Set environment variables: Create a .env file (Python) or .properties (Java) in your project's root directory and add the following lines. ADK will automatically load this file.


export GOOGLE_API_KEY="YOUR_GOOGLE_API_KEY"
export GOOGLE_GENAI_USE_VERTEXAI=FALSE
(or)

Pass these variables during the model initialization via the Client (see example below).

Models: Find all available models on the Google AI for Developers site.

Google Cloud Vertex AI¶
For scalable and production-oriented use cases, Vertex AI is the recommended platform. Gemini on Vertex AI supports enterprise-grade features, security, and compliance controls. Based on your development environment and usecase, choose one of the below methods to authenticate.

Pre-requisites: A Google Cloud Project with Vertex AI enabled.

Method A: User Credentials (for Local Development)¶
Install the gcloud CLI: Follow the official installation instructions.
Log in using ADC: This command opens a browser to authenticate your user account for local development.

gcloud auth application-default login
Set environment variables:


export GOOGLE_CLOUD_PROJECT="YOUR_PROJECT_ID"
export GOOGLE_CLOUD_LOCATION="YOUR_VERTEX_AI_LOCATION" # e.g., us-central1
Explicitly tell the library to use Vertex AI:


export GOOGLE_GENAI_USE_VERTEXAI=TRUE
Models: Find available model IDs in the Vertex AI documentation.

Method B: Vertex AI Express Mode¶
Vertex AI Express Mode offers a simplified, API-key-based setup for rapid prototyping.

Sign up for Express Mode to get your API key.
Set environment variables:

export GOOGLE_API_KEY="PASTE_YOUR_EXPRESS_MODE_API_KEY_HERE"
export GOOGLE_GENAI_USE_VERTEXAI=TRUE
Method C: Service Account (for Production & Automation)¶
For deployed applications, a service account is the standard method.

Create a Service Account and grant it the Vertex AI User role.
Provide credentials to your application:
On Google Cloud: If you are running the agent in Cloud Run, GKE, VM or other Google Cloud services, the environment can automatically provide the service account credentials. You don't have to create a key file.
Elsewhere: Create a service account key file and point to it with an environment variable:

export GOOGLE_APPLICATION_CREDENTIALS="/path/to/your/keyfile.json"
Instead of the key file, you can also authenticate the service account using Workload Identity. But this is outside the scope of this guide.
Example:


Python
Go
Java

import (
    "google.golang.org/adk/agent/llmagent"
    "google.golang.org/adk/model/gemini"
    "google.golang.org/genai"
)

// --- Example using a stable Gemini Flash model ---
modelFlash, err := gemini.NewModel(ctx, "gemini-2.0-flash", &genai.ClientConfig{})
if err != nil {
    log.Fatalf("failed to create model: %v", err)
}
agentGeminiFlash, err := llmagent.New(llmagent.Config{
    // Use the latest stable Flash model identifier
    Model:       modelFlash,
    Name:        "gemini_flash_agent",
    Instruction: "You are a fast and helpful Gemini assistant.",
    // ... other agent parameters
})
if err != nil {
    log.Fatalf("failed to create agent: %v", err)
}

// --- Example using a powerful Gemini Pro model ---
// Note: Always check the official Gemini documentation for the latest model names,
// including specific preview versions if needed. Preview models might have
// different availability or quota limitations.
modelPro, err := gemini.NewModel(ctx, "gemini-2.5-pro-preview-03-25", &genai.ClientConfig{})
if err != nil {
    log.Fatalf("failed to create model: %v", err)
}
agentGeminiPro, err := llmagent.New(llmagent.Config{
    // Use the latest generally available Pro model identifier
    Model:       modelPro,
    Name:        "gemini_pro_agent",
    Instruction: "You are a powerful and knowledgeable Gemini assistant.",
    // ... other agent parameters
})
if err != nil {
    log.Fatalf("failed to create agent: %v", err)
}

Secure Your Credentials

Service account credentials or API keys are powerful credentials. Never expose them publicly. Use a secret manager such as Google Cloud Secret Manager to store and access them securely in production.

Troubleshooting¶
Error Code 429 - RESOURCE_EXHAUSTED¶
This error usually happens if the number of your requests exceeds the capacity allocated to process requests.

To mitigate this, you can do one of the following:

Request higher quota limits for the model you are trying to use.

Enable client-side retries. Retries allow the client to automatically retry the request after a delay, which can help if the quota issue is temporary.

There are two ways you can set retry options:

Option 1: Set retry options on the Agent as a part of generate_content_config.

You would use this option if you are instantiating this model adapter by yourself.


root_agent = Agent(
    model='gemini-2.0-flash',
    ...
    generate_content_config=types.GenerateContentConfig(
        ...
        http_options=types.HttpOptions(
            ...
            retry_options=types.HttpRetryOptions(initial_delay=1, attempts=2),
            ...
        ),
        ...
    )
Option 2: Retry options on this model adapter.

You would use this option if you were instantiating the instance of adapter by yourself.


from google.genai import types

# ...

agent = Agent(
    model=Gemini(
    retry_options=types.HttpRetryOptions(initial_delay=1, attempts=2),
    )
)