//! ADK Studio Codegen Demo
//!
//! Demonstrates generating Rust code from project schemas (templates).
//! Run: cargo run -p adk-studio --example codegen_demo

use adk_studio::codegen::generate_rust_project;
use adk_studio::schema::{AgentSchema, AgentType, Edge, ProjectSchema, Route};
use std::fs;

fn main() {
    println!("🔧 ADK Studio Codegen Demo\n");
    println!("This demo generates Rust projects from agent templates.\n");

    // Generate all template examples
    let templates = vec![
        ("simple_chat", simple_chat_project()),
        ("research_pipeline", research_pipeline_project()),
        ("content_refiner", content_refiner_project()),
        ("parallel_analyzer", parallel_analyzer_project()),
        ("support_router", support_router_project()),
    ];

    for (name, project) in templates {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("📦 Generating: {}", name);
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

        match generate_rust_project(&project) {
            Ok(generated) => {
                for file in &generated.files {
                    println!("📄 {}\n", file.path);
                    // Print first 60 lines of main.rs
                    if file.path == "src/main.rs" {
                        let lines: Vec<&str> = file.content.lines().take(60).collect();
                        for line in lines {
                            println!("  {}", line);
                        }
                        println!("  ... (truncated)\n");
                    }
                }

                // Optionally write to disk
                let out_dir = format!("/tmp/adk-codegen-demo/{}", name);
                fs::create_dir_all(format!("{}/src", out_dir)).ok();
                for file in &generated.files {
                    let path = format!("{}/{}", out_dir, file.path);
                    fs::write(&path, &file.content).ok();
                }
                println!("✅ Written to: {}\n", out_dir);
            }
            Err(e) => println!("❌ Error: {}\n", e),
        }
    }

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("🚀 To run an example:");
    println!("   cd /tmp/adk-codegen-demo/simple_chat");
    println!("   GOOGLE_API_KEY=your_key cargo run");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
}

// ============================================================================
// Template Projects
// ============================================================================

fn simple_chat_project() -> ProjectSchema {
    let mut p = ProjectSchema::new("simple_chat");
    p.agents.insert(
        "chat_agent".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.5-flash".to_string()),
            instruction:
                "You are a helpful, friendly assistant. Answer questions clearly and concisely."
                    .to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    p.workflow.edges.push(Edge::new("START", "chat_agent"));
    p.workflow.edges.push(Edge::new("chat_agent", "END"));
    p
}

fn research_pipeline_project() -> ProjectSchema {
    let mut p = ProjectSchema::new("research_pipeline");

    p.agents.insert(
        "researcher".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.5-flash".to_string()),
            instruction:
                "Research the topic using Google Search. Gather key facts and recent developments."
                    .to_string(),
            tools: vec!["google_search".to_string()],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    p.agents.insert(
        "summarizer".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.5-flash".to_string()),
            instruction: "Summarize the research into key takeaways and conclusions.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    p.agents.insert(
        "pipeline".to_string(),
        AgentSchema {
            agent_type: AgentType::Sequential,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec!["researcher".to_string(), "summarizer".to_string()],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    p.workflow.edges.push(Edge::new("START", "pipeline"));
    p.workflow.edges.push(Edge::new("pipeline", "END"));
    p
}

fn content_refiner_project() -> ProjectSchema {
    let mut p = ProjectSchema::new("content_refiner");

    p.agents.insert(
        "improver".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.5-flash".to_string()),
            instruction: "Improve the content: fix errors, enhance clarity, improve flow."
                .to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    p.agents.insert(
        "reviewer".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.5-flash".to_string()),
            instruction:
                "Review content quality. Call exit_loop when polished, otherwise continue."
                    .to_string(),
            tools: vec!["exit_loop".to_string()],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    p.agents.insert(
        "refiner".to_string(),
        AgentSchema {
            agent_type: AgentType::Loop,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec!["improver".to_string(), "reviewer".to_string()],
            position: Default::default(),
            max_iterations: Some(3),
            routes: vec![],
        },
    );

    p.workflow.edges.push(Edge::new("START", "refiner"));
    p.workflow.edges.push(Edge::new("refiner", "END"));
    p
}

fn parallel_analyzer_project() -> ProjectSchema {
    let mut p = ProjectSchema::new("parallel_analyzer");

    p.agents.insert(
        "sentiment".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.5-flash".to_string()),
            instruction: "Analyze sentiment: positive/negative/neutral with key emotional tones."
                .to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    p.agents.insert(
        "entities".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.5-flash".to_string()),
            instruction: "Extract entities: people, organizations, locations, dates.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    p.agents.insert(
        "analyzer".to_string(),
        AgentSchema {
            agent_type: AgentType::Parallel,
            model: None,
            instruction: String::new(),
            tools: vec![],
            sub_agents: vec!["sentiment".to_string(), "entities".to_string()],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    p.workflow.edges.push(Edge::new("START", "analyzer"));
    p.workflow.edges.push(Edge::new("analyzer", "END"));
    p
}

fn support_router_project() -> ProjectSchema {
    let mut p = ProjectSchema::new("support_router");

    p.agents.insert(
        "router".to_string(),
        AgentSchema {
            agent_type: AgentType::Router,
            model: Some("gemini-2.5-flash".to_string()),
            instruction: "Classify request as: technical, billing, or general.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![
                Route { condition: "technical".to_string(), target: "tech_agent".to_string() },
                Route { condition: "billing".to_string(), target: "billing_agent".to_string() },
                Route { condition: "general".to_string(), target: "general_agent".to_string() },
            ],
        },
    );

    p.agents.insert(
        "tech_agent".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.5-flash".to_string()),
            instruction: "You are technical support. Help with coding and bugs.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    p.agents.insert(
        "billing_agent".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.5-flash".to_string()),
            instruction: "You are billing support. Help with payments and subscriptions."
                .to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );
    p.agents.insert(
        "general_agent".to_string(),
        AgentSchema {
            agent_type: AgentType::Llm,
            model: Some("gemini-2.5-flash".to_string()),
            instruction: "You are general support. Help with general questions.".to_string(),
            tools: vec![],
            sub_agents: vec![],
            position: Default::default(),
            max_iterations: None,
            routes: vec![],
        },
    );

    p.workflow.edges.push(Edge::new("START", "router"));
    p.workflow.edges.push(Edge {
        from: "router".to_string(),
        to: "tech_agent".to_string(),
        condition: Some("technical".to_string()),
        from_port: None,
        to_port: None,
    });
    p.workflow.edges.push(Edge {
        from: "router".to_string(),
        to: "billing_agent".to_string(),
        condition: Some("billing".to_string()),
        from_port: None,
        to_port: None,
    });
    p.workflow.edges.push(Edge {
        from: "router".to_string(),
        to: "general_agent".to_string(),
        condition: Some("general".to_string()),
        from_port: None,
        to_port: None,
    });
    p.workflow.edges.push(Edge::new("tech_agent", "END"));
    p.workflow.edges.push(Edge::new("billing_agent", "END"));
    p.workflow.edges.push(Edge::new("general_agent", "END"));
    p
}
