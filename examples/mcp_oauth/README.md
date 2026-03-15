# MCP OAuth Authentication Example

This example demonstrates connecting to authenticated MCP servers using bearer tokens or OAuth2, specifically showcasing the GitHub Copilot MCP server integration.

## Features

- Bearer token authentication (GitHub PAT)
- OAuth2 client credentials flow
- API key authentication
- GitHub Copilot MCP server with 40+ tools

## Prerequisites

- Rust 1.85+
- `GOOGLE_API_KEY` environment variable (for the agent)
- `GITHUB_TOKEN` environment variable (GitHub Personal Access Token)

## GitHub Token Setup

Create a GitHub Personal Access Token with the following scopes:
- `repo` - Full control of private repositories
- `user` - Read user profile data
- `gist` - Create gists (optional)

## Running

```bash
# Set environment variables in .env file or export them
export GITHUB_TOKEN=ghp_xxxxxxxxxxxx
export GOOGLE_API_KEY=your_google_api_key

# Run the example
cargo run --example mcp_oauth --features http-transport
```

## Available GitHub Tools (40+)

When connected to GitHub Copilot MCP, you get access to:

### Repository Management
- `create_repository` - Create new repositories
- `fork_repository` - Fork repositories
- `create_branch` - Create branches
- `list_branches` - List branches

### Issues
- `issue_read` / `issue_write` - Read and create issues
- `add_issue_comment` - Comment on issues
- `assign_copilot_to_issue` - Assign Copilot to issues
- `search_issues` - Search issues

### Pull Requests
- `create_pull_request` - Create PRs
- `pull_request_read` - Read PR details
- `merge_pull_request` - Merge PRs
- `pull_request_review_write` - Create reviews
- `request_copilot_review` - Request Copilot code review

### Code & Files
- `get_file_contents` - Read files
- `create_or_update_file` - Write files
- `push_files` - Push multiple files
- `search_code` - Search code across GitHub

### Search
- `search_repositories` - Find repositories
- `search_users` - Find users
- `search_pull_requests` - Find PRs

## Authentication Methods

### 1. Bearer Token (GitHub PAT)

```rust
use adk_tool::{McpHttpClientBuilder, McpAuth};

let toolset = McpHttpClientBuilder::new("https://api.githubcopilot.com/mcp/")
    .with_auth(McpAuth::bearer("ghp_xxxxxxxxxxxx"))
    .connect()
    .await?;
```

### 2. OAuth2 Client Credentials

```rust
use adk_tool::{McpHttpClientBuilder, McpAuth, OAuth2Config};

let oauth_config = OAuth2Config::new(
    "client-id",
    "https://auth.example.com/oauth/token"
)
.with_secret("client-secret")
.with_scopes(vec!["mcp:read".into()]);

let toolset = McpHttpClientBuilder::new("https://api.example.com/mcp/")
    .with_auth(McpAuth::oauth2(oauth_config))
    .connect()
    .await?;
```

## Example Session

```
MCP OAuth Authentication Example
=================================

Connecting to GitHub Copilot MCP server...
Endpoint: https://api.githubcopilot.com/mcp/

✅ Connected to GitHub Copilot MCP server!

Discovered 40 tools:
  - create_repository: Create a new GitHub repository...
  - create_pull_request: Create a new pull request...
  - search_code: Fast and precise code search...
  ...

✅ Agent created with MCP tools

Starting interactive console...

User -> List my repositories
Agent -> [Uses list_repositories tool to fetch and display repos]
```
