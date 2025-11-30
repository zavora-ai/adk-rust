MCP Toolbox for Databases¶
Supported in ADKPythonGo
MCP Toolbox for Databases is an open source MCP server for databases. It was designed with enterprise-grade and production-quality in mind. It enables you to develop tools easier, faster, and more securely by handling the complexities such as connection pooling, authentication, and more.

Google’s Agent Development Kit (ADK) has built in support for Toolbox. For more information on getting started or configuring Toolbox, see the documentation.

GenAI Toolbox

Supported Data Sources¶
MCP Toolbox provides out-of-the-box toolsets for the following databases and data platforms:

Google Cloud¶
BigQuery (including tools for SQL execution, schema discovery, and AI-powered time series forecasting)
AlloyDB (PostgreSQL-compatible, with tools for both standard queries and natural language queries)
AlloyDB Admin
Spanner (supporting both GoogleSQL and PostgreSQL dialects)
Cloud SQL (with dedicated support for Cloud SQL for PostgreSQL, Cloud SQL for MySQL, and Cloud SQL for SQL Server)
Cloud SQL Admin
Firestore
Bigtable
Dataplex (for data discovery and metadata search)
Cloud Monitoring
Relational & SQL Databases¶
PostgreSQL (generic)
MySQL (generic)
Microsoft SQL Server (generic)
ClickHouse
TiDB
OceanBase
Firebird
SQLite
YugabyteDB
NoSQL & Key-Value Stores¶
MongoDB
Couchbase
Redis
Valkey
Cassandra
Graph Databases¶
Neo4j (with tools for Cypher queries and schema inspection)
Dgraph
Data Platforms & Federation¶
Looker (for running Looks, queries, and building dashboards via the Looker API)
Trino (for running federated queries across multiple sources)
Other¶
HTTP
Configure and deploy¶
Toolbox is an open source server that you deploy and manage yourself. For more instructions on deploying and configuring, see the official Toolbox documentation:

Installing the Server
Configuring Toolbox
Install Client SDK for ADK¶

Python
Go
ADK relies on the mcp-toolbox-sdk-go go module to use Toolbox. Install the module before getting started:


go get github.com/googleapis/mcp-toolbox-sdk-go
Loading Toolbox Tools¶
Once you’re Toolbox server is configured and up and running, you can load tools from your server using ADK:


package main

import (
    "context"
    "fmt"

    "github.com/googleapis/mcp-toolbox-sdk-go/tbadk"
    "google.golang.org/adk/agent/llmagent"
)

func main() {

  toolboxClient, err := tbadk.NewToolboxClient("https://127.0.0.1:5000")
    if err != nil {
        log.Fatalf("Failed to create MCP Toolbox client: %v", err)
    }

  // Load a specific set of tools
  toolboxtools, err := toolboxClient.LoadToolset("my-toolset-name", ctx)
  if err != nil {
    return fmt.Sprintln("Could not load Toolbox Toolset", err)
  }

  toolsList := make([]tool.Tool, len(toolboxtools))
    for i := range toolboxtools {
      toolsList[i] = &toolboxtools[i]
    }

  llmagent, err := llmagent.New(llmagent.Config{
    ...,
    Tools:       toolsList,
  })

  // Load a single tool
  tool, err := client.LoadTool("my-tool-name", ctx)
  if err != nil {
    return fmt.Sprintln("Could not load Toolbox Tool", err)
  }

  llmagent, err := llmagent.New(llmagent.Config{
    ...,
    Tools:       []tool.Tool{&toolboxtool},
  })
}

Advanced Toolbox Features¶
Toolbox has a variety of features to make developing Gen AI tools for databases. For more information, read more about the following features:

Authenticated Parameters: bind tool inputs to values from OIDC tokens automatically, making it easy to run sensitive queries without potentially leaking data
Authorized Invocations: restrict access to use a tool based on the users Auth token
OpenTelemetry: get metrics and tracing from Toolbox with OpenTelemetry