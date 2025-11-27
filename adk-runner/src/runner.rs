use crate::InvocationContext;
use adk_core::{Agent, Artifacts, Content, EventStream, Memory, Result};
use adk_session::SessionService;
use async_stream::stream;
use std::sync::Arc;

pub struct RunnerConfig {
    pub app_name: String,
    pub agent: Arc<dyn Agent>,
    pub session_service: Arc<dyn SessionService>,
    pub artifact_service: Option<Arc<dyn Artifacts>>,
    pub memory_service: Option<Arc<dyn Memory>>,
}

pub struct Runner {
    app_name: String,
    root_agent: Arc<dyn Agent>,
    session_service: Arc<dyn SessionService>,
    artifact_service: Option<Arc<dyn Artifacts>>,
    memory_service: Option<Arc<dyn Memory>>,
}

impl Runner {
    pub fn new(config: RunnerConfig) -> Result<Self> {
        Ok(Self {
            app_name: config.app_name,
            root_agent: config.agent,
            session_service: config.session_service,
            artifact_service: config.artifact_service,
            memory_service: config.memory_service,
        })
    }

    pub async fn run(
        &self,
        user_id: String,
        session_id: String,
        user_content: Content,
    ) -> Result<EventStream> {
        let app_name = self.app_name.clone();
        let session_service = self.session_service.clone();
        let root_agent = self.root_agent.clone();
        let artifact_service = self.artifact_service.clone();
        let memory_service = self.memory_service.clone();

        let s = stream! {
            // Get or create session
            let session = match session_service
                .get(adk_session::GetRequest {
                    app_name: app_name.clone(),
                    user_id: user_id.clone(),
                    session_id: session_id.clone(),
                    num_recent_events: None,
                    after: None,
                })
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            // Find which agent should handle this request
            let agent_to_run = Self::find_agent_to_run(&root_agent, session.as_ref());

            // Create invocation context
            let invocation_id = format!("inv-{}", uuid::Uuid::new_v4());
            let mut ctx = InvocationContext::new(
                invocation_id.clone(),
                agent_to_run.clone(),
                user_id.clone(),
                app_name.clone(),
                session_id.clone(),
                user_content.clone(),
                Arc::from(session),
            );

            // Add optional services
            if let Some(artifacts) = artifact_service {
                ctx = ctx.with_artifacts(artifacts);
            }
            if let Some(memory) = memory_service {
                ctx = ctx.with_memory(memory);
            }

            let ctx = Arc::new(ctx);

            // Append user message to session
            let mut user_event = adk_core::Event::new(&invocation_id);
            user_event.author = "user".to_string();
            user_event.content = Some(user_content.clone());
            
            // Convert to session event
            let session_event = adk_session::Event {
                id: user_event.id.clone(),
                timestamp: user_event.timestamp,
                invocation_id: user_event.invocation_id.clone(),
                branch: user_event.branch.clone(),
                author: user_event.author.clone(),
                llm_response: adk_core::LlmResponse::new(user_content.clone()),
                actions: adk_session::EventActions {
                    state_delta: user_event.actions.state_delta.clone(),
                    artifact_delta: user_event.actions.artifact_delta.clone(),
                    skip_summarization: false,
                    transfer_to_agent: None,
                    escalate: user_event.actions.escalate,
                },
                long_running_tool_ids: vec![],
            };
            
            if let Err(e) = session_service.append_event(&session_id, session_event).await {
                yield Err(e);
                return;
            }

            // Run the agent
            let mut agent_stream = match agent_to_run.run(ctx).await {
                Ok(s) => s,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            // Stream events
            use futures::StreamExt;
            while let Some(result) = agent_stream.next().await {
                match result {
                    Ok(event) => {
                        // Convert to session event and append
                        let session_event = adk_session::Event {
                            id: event.id.clone(),
                            timestamp: event.timestamp,
                            invocation_id: event.invocation_id.clone(),
                            branch: event.branch.clone(),
                            author: event.author.clone(),
                            llm_response: adk_core::LlmResponse::new(
                                event.content.clone().unwrap_or_else(|| adk_core::Content::new("model"))
                            ),
                            actions: adk_session::EventActions {
                                state_delta: event.actions.state_delta.clone(),
                                artifact_delta: event.actions.artifact_delta.clone(),
                                skip_summarization: false,
                                transfer_to_agent: None,
                                escalate: event.actions.escalate,
                            },
                            long_running_tool_ids: vec![],
                        };
                        
                        if let Err(e) = session_service.append_event(&session_id, session_event).await {
                            yield Err(e);
                            return;
                        }
                        yield Ok(event);
                    }
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }
        };

        Ok(Box::pin(s))
    }

    /// Find which agent should handle the request based on session history
    pub fn find_agent_to_run(root_agent: &Arc<dyn Agent>, session: &dyn adk_session::Session) -> Arc<dyn Agent> {
        // Look at recent events to find last agent that responded
        let events = session.events();
        for i in (0..events.len()).rev() {
            if let Some(event) = events.at(i) {
                if event.author == "user" {
                    continue;
                }

                // Try to find this agent in the tree
                if let Some(agent) = Self::find_agent(root_agent, &event.author) {
                    // Check if agent allows transfer up the tree
                    if Self::is_transferable(root_agent, &agent) {
                        return agent;
                    }
                }
            }
        }

        // Default to root agent
        root_agent.clone()
    }

    /// Check if agent and its parent chain allow transfer up the tree
    fn is_transferable(root_agent: &Arc<dyn Agent>, agent: &Arc<dyn Agent>) -> bool {
        // For now, always allow transfer
        // TODO: Check DisallowTransferToParent flag when LlmAgent supports it
        let _ = (root_agent, agent);
        true
    }

    /// Recursively search agent tree for agent with given name
    pub fn find_agent(current: &Arc<dyn Agent>, target_name: &str) -> Option<Arc<dyn Agent>> {
        if current.name() == target_name {
            return Some(current.clone());
        }

        for sub_agent in current.sub_agents() {
            if let Some(found) = Self::find_agent(sub_agent, target_name) {
                return Some(found);
            }
        }

        None
    }
}
