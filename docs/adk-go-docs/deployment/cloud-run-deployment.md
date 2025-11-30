Deploy to Cloud Run¶
Supported in ADKPythonGoJava
Cloud Run is a fully managed platform that enables you to run your code directly on top of Google's scalable infrastructure.

To deploy your agent, you can use either the adk deploy cloud_run command (recommended for Python), or with gcloud run deploy command through Cloud Run.

Agent sample¶
For each of the commands, we will reference a the Capital Agent sample defined on the LLM agent page. We will assume it's in a directory (eg: capital_agent).

To proceed, confirm that your agent code is configured as follows:


Python
Go
Java
Your application's entry point (the main package and main() function) is in a single Go file. Using main.go is a strong convention.
Your agent instance is passed to a launcher configuration, typically using agent.NewSingleLoader(yourAgent). The adkgo tool uses this launcher to start your agent with the correct services.
Your go.mod and go.sum files are present in your project directory to manage dependencies.
Refer to the following section for more details. You can also find a sample app in the Github repo.


Environment variables¶
Set your environment variables as described in the Setup and Installation guide.


export GOOGLE_CLOUD_PROJECT=your-project-id
export GOOGLE_CLOUD_LOCATION=us-central1 # Or your preferred location
export GOOGLE_GENAI_USE_VERTEXAI=True
(Replace your-project-id with your actual GCP project ID)

Alternatively you can also use an API key from AI Studio


export GOOGLE_CLOUD_PROJECT=your-project-id
export GOOGLE_CLOUD_LOCATION=us-central1 # Or your preferred location
export GOOGLE_GENAI_USE_VERTEXAI=FALSE
export GOOGLE_API_KEY=your-api-key
(Replace your-project-id with your actual GCP project ID and your-api-key with your actual API key from AI Studio)
Prerequisites¶
You should have a Google Cloud project. You need to know your:
Project name (i.e. "my-project")
Project location (i.e. "us-central1")
Service account (i.e. "1234567890-compute@developer.gserviceaccount.com")
GOOGLE_API_KEY
Secret¶
Please make sure you have created a secret which can be read by your service account.

Entry for GOOGLE_API_KEY secret¶
You can create your secret manually or use CLI:


echo "<<put your GOOGLE_API_KEY here>>" | gcloud secrets create GOOGLE_API_KEY --project=my-project --data-file=-
Permissions to read¶
You should give appropiate permissision for you service account to read this secret.


gcloud secrets add-iam-policy-binding GOOGLE_API_KEY --member="serviceAccount:1234567890-compute@developer.gserviceaccount.com" --role="roles/secretmanager.secretAccessor" --project=my-project
Deployment payload¶
When you deploy your ADK agent workflow to the Google Cloud Run, the following content is uploaded to the service:

Your ADK agent code
Any dependencies declared in your ADK agent code
ADK API server code version used by your agent
The default deployment does not include the ADK web user interface libraries, unless you specify it as deployment setting, such as the --with_ui option for adk deploy cloud_run command.

Deployment commands¶

Python - adk CLI
Python - gcloud CLI
Go - adkgo CLI
Java - gcloud CLI
adk CLI¶
The adk deploy cloud_run command deploys your agent code to Google Cloud Run.

Ensure you have authenticated with Google Cloud (gcloud auth login and gcloud config set project <your-project-id>).

Setup environment variables¶
Optional but recommended: Setting environment variables can make the deployment commands cleaner.


# Set your Google Cloud Project ID
export GOOGLE_CLOUD_PROJECT="your-gcp-project-id"

# Set your desired Google Cloud Location
export GOOGLE_CLOUD_LOCATION="us-central1" # Example location

# Set the path to your agent code directory
export AGENT_PATH="./capital_agent" # Assuming capital_agent is in the current directory

# Set a name for your Cloud Run service (optional)
export SERVICE_NAME="capital-agent-service"

# Set an application name (optional)
export APP_NAME="capital-agent-app"
Command usage¶
Minimal command¶

adk deploy cloud_run \
--project=$GOOGLE_CLOUD_PROJECT \
--region=$GOOGLE_CLOUD_LOCATION \
$AGENT_PATH
Full command with optional flags¶

adk deploy cloud_run \
--project=$GOOGLE_CLOUD_PROJECT \
--region=$GOOGLE_CLOUD_LOCATION \
--service_name=$SERVICE_NAME \
--app_name=$APP_NAME \
--with_ui \
$AGENT_PATH
Arguments¶
AGENT_PATH: (Required) Positional argument specifying the path to the directory containing your agent's source code (e.g., $AGENT_PATH in the examples, or capital_agent/). This directory must contain at least an __init__.py and your main agent file (e.g., agent.py).
Options¶
--project TEXT: (Required) Your Google Cloud project ID (e.g., $GOOGLE_CLOUD_PROJECT).
--region TEXT: (Required) The Google Cloud location for deployment (e.g., $GOOGLE_CLOUD_LOCATION, us-central1).
--service_name TEXT: (Optional) The name for the Cloud Run service (e.g., $SERVICE_NAME). Defaults to adk-default-service-name.
--app_name TEXT: (Optional) The application name for the ADK API server (e.g., $APP_NAME). Defaults to the name of the directory specified by AGENT_PATH (e.g., capital_agent if AGENT_PATH is ./capital_agent).
--agent_engine_id TEXT: (Optional) If you are using a managed session service via Vertex AI Agent Engine, provide its resource ID here.
--port INTEGER: (Optional) The port number the ADK API server will listen on within the container. Defaults to 8000.
--with_ui: (Optional) If included, deploys the ADK dev UI alongside the agent API server. By default, only the API server is deployed.
--temp_folder TEXT: (Optional) Specifies a directory for storing intermediate files generated during the deployment process. Defaults to a timestamped folder in the system's temporary directory. (Note: This option is generally not needed unless troubleshooting issues).
--help: Show the help message and exit.
Authenticated access¶
During the deployment process, you might be prompted: Allow unauthenticated invocations to [your-service-name] (y/N)?.

Enter y to allow public access to your agent's API endpoint without authentication.
Enter N (or press Enter for the default) to require authentication (e.g., using an identity token as shown in the "Testing your agent" section).
Upon successful execution, the command deploys your agent to Cloud Run and provide the URL of the deployed service.


Testing your agent¶
Once your agent is deployed to Cloud Run, you can interact with it via the deployed UI (if enabled) or directly with its API endpoints using tools like curl. You'll need the service URL provided after deployment.


UI Testing
API Testing (curl)
UI Testing¶
If you deployed your agent with the UI enabled:

adk CLI: You included the --webui flag during deployment.
gcloud CLI: You set SERVE_WEB_INTERFACE = True in your main.py.
You can test your agent by simply navigating to the Cloud Run service URL provided after deployment in your web browser.


# Example URL format
# https://your-service-name-abc123xyz.a.run.app
The ADK dev UI allows you to interact with your agent, manage sessions, and view execution details directly in the browser.

To verify your agent is working as intended, you can:

Select your agent from the dropdown menu.
Type a message and verify that you receive an expected response from your agent.
If you experience any unexpected behavior, check the Cloud Run console logs.