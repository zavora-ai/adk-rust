use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
};

use crate::types::{
    AgentActionResponse, AgentScaleRequest, AlertEvent, AlertRule, AnnotationQueue,
    ApiExplorerResponse, ApiKeySummary, AuditExportResponse, BillingUsageItem, CatalogTemplate,
    ChangeTierRequest, CreateAlertRuleRequest, CreateApiKeyRequest, CreateApiKeyResponse,
    CreateEnvironmentRequest, EndpointDoc, EvaluationRun, EvaluationsResponse, HitlDecisionRequest,
    InviteTeamMemberRequest, PromoteEnvironmentRequest, RunEvaluationRequest, TeamMember,
    WorkspaceResponse,
};
use adk_deploy::{
    ActiveInstance, AgentDetail, AgentSummary, AlertSummary, AuditEvent, BillingSummary,
    DashboardResponse, DeploymentHistoryResponse, DeploymentRecord, DeploymentStatusResponse,
    DeploymentStatusValue, DeploymentSummary, EnvironmentSummary, HitlCheckpoint, LogEntry,
    LoginRequest, LoginResponse, MetricPoint, MetricsSummary, PushDeploymentRequest,
    SecretListResponse, SecretSetRequest, TraceSummary, WorkspaceSummary,
};
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::Utc;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug)]
pub struct PlatformState {
    path: PathBuf,
    cipher: SecretCipher,
    data: RwLock<PersistedState>,
}

#[derive(Debug, Clone)]
pub struct AuthorizedToken {
    pub workspace_id: String,
    pub principal: String,
    pub scopes: Vec<String>,
}

impl PlatformState {
    pub(crate) async fn load() -> Result<Self, StateError> {
        let base = platform_data_dir()?;
        Self::load_from_dir(base).await
    }

    pub(crate) async fn load_from_dir(base_dir: PathBuf) -> Result<Self, StateError> {
        let path = base_dir.join("state.json");
        let cipher = SecretCipher::load_from_dir(&base_dir)?;
        let mut data = if path.exists() {
            let raw = fs::read_to_string(&path)?;
            serde_json::from_str::<PersistedState>(&raw)?
        } else {
            PersistedState::default_seeded()
        };
        data.hydrate_missing();
        let state = Self { path, cipher, data: RwLock::new(data) };
        state.persist().await?;
        Ok(state)
    }

    pub async fn login(&self, request: LoginRequest) -> Result<LoginResponse, String> {
        let mut data = self.data.write().await;
        let workspace = if let Some(name) = request.workspace_name {
            ensure_workspace(&mut data, &name)
        } else {
            data.workspaces
                .first()
                .cloned()
                .unwrap_or_else(|| ensure_workspace(&mut data, "Default Workspace"))
        };
        let token = format!("adk_{:x}", Uuid::new_v4());
        data.sessions.insert(token.clone(), workspace.id.clone());
        data.audit_events.push(AuditEvent {
            timestamp: now_string(),
            action: "login".to_string(),
            resource: workspace.id.clone(),
            result: format!("success ({})", request.email),
        });
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(LoginResponse { token, workspace_id: workspace.id })
    }

    pub async fn authorize_token(&self, token: &str) -> Result<AuthorizedToken, String> {
        let data = self.data.read().await;
        if let Some(workspace_id) = data.sessions.get(token) {
            return Ok(AuthorizedToken {
                workspace_id: workspace_id.clone(),
                principal: "dev-session".to_string(),
                scopes: vec![
                    "deploy:read".to_string(),
                    "deploy:write".to_string(),
                    "deploy:admin".to_string(),
                ],
            });
        }
        if let Some(api_key) = data.api_keys.iter().find(|api_key| api_key.matches(token)) {
            let workspace_id = if api_key.workspace_id.is_empty() {
                data.workspaces
                    .first()
                    .map(|workspace| workspace.id.clone())
                    .ok_or_else(|| "workspace not found".to_string())?
            } else {
                api_key.workspace_id.clone()
            };
            return Ok(AuthorizedToken {
                workspace_id,
                principal: format!("api-key:{}", api_key.id),
                scopes: api_key.scopes.clone(),
            });
        }
        Err("unauthorized".to_string())
    }

    pub async fn dashboard(&self) -> Result<DashboardResponse, String> {
        let workspace_id = self.default_workspace_id().await?;
        self.dashboard_for_workspace(&workspace_id).await
    }

    pub async fn dashboard_for_workspace(
        &self,
        workspace_id: &str,
    ) -> Result<DashboardResponse, String> {
        let data = self.data.read().await;
        let workspace = data
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .cloned()
            .ok_or_else(|| format!("workspace not found: {workspace_id}"))?;
        let agents = current_agent_summaries(&data, Some(workspace_id));
        let environments = current_environments(&data);
        Ok(DashboardResponse {
            workspace,
            traces: synthetic_traces(&agents),
            logs: synthetic_logs(&agents),
            hitl: data.hitl_items.clone(),
            usage: synthetic_usage(&agents),
            alerts: current_alert_summaries(&data, &agents),
            active_strategy: data
                .deployments
                .last()
                .map(|deployment| deployment.strategy)
                .unwrap_or(adk_deploy::DeploymentStrategyKind::Rolling),
            agents,
            environments,
        })
    }

    pub async fn default_workspace_id(&self) -> Result<String, String> {
        let data = self.data.read().await;
        data.workspaces
            .first()
            .map(|workspace| workspace.id.clone())
            .ok_or_else(|| "workspace not found".to_string())
    }

    pub async fn workspace_summary(&self, workspace_id: &str) -> Result<WorkspaceSummary, String> {
        let data = self.data.read().await;
        data.workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .cloned()
            .ok_or_else(|| format!("workspace not found: {workspace_id}"))
    }

    pub async fn list_agents(&self) -> Result<Vec<AgentSummary>, String> {
        let data = self.data.read().await;
        Ok(current_agent_summaries(&data, None))
    }

    pub async fn list_environments(&self) -> Result<Vec<EnvironmentSummary>, String> {
        let data = self.data.read().await;
        Ok(current_environments(&data))
    }

    pub async fn traces(&self) -> Result<Vec<TraceSummary>, String> {
        let data = self.data.read().await;
        let agents = current_agent_summaries(&data, None);
        Ok(synthetic_traces(&agents))
    }

    pub async fn logs(&self) -> Result<Vec<LogEntry>, String> {
        let data = self.data.read().await;
        let agents = current_agent_summaries(&data, None);
        Ok(synthetic_logs(&agents))
    }

    pub async fn hitl(&self) -> Result<Vec<HitlCheckpoint>, String> {
        let data = self.data.read().await;
        Ok(data.hitl_items.clone())
    }

    pub async fn alerts(&self) -> Result<Vec<AlertSummary>, String> {
        let data = self.data.read().await;
        let agents = current_agent_summaries(&data, None);
        Ok(current_alert_summaries(&data, &agents))
    }

    pub async fn billing(&self) -> Result<Vec<BillingSummary>, String> {
        let data = self.data.read().await;
        let agents = current_agent_summaries(&data, None);
        Ok(synthetic_usage(&agents))
    }

    pub async fn billing_usage(&self) -> Result<Vec<BillingUsageItem>, String> {
        let data = self.data.read().await;
        let plan = data
            .workspaces
            .first()
            .map(|workspace| workspace.plan.clone())
            .unwrap_or_else(|| "Pro".to_string());
        let agents = current_agent_summaries(&data, None);
        Ok(vec![
            BillingUsageItem {
                label: "Agents".to_string(),
                current: agents.len().to_string(),
                limit: if plan == "Free" { "3".to_string() } else { "Unlimited".to_string() },
                unit: "deployments".to_string(),
            },
            BillingUsageItem {
                label: "Traces".to_string(),
                current: format!("{}k", agents.len() * 12),
                limit: if plan == "Enterprise" {
                    "Unlimited".to_string()
                } else if plan == "Pro" {
                    "50k".to_string()
                } else {
                    "5k".to_string()
                },
                unit: "monthly traces".to_string(),
            },
            BillingUsageItem {
                label: "Retention".to_string(),
                current: if plan == "Enterprise" {
                    "90".to_string()
                } else if plan == "Pro" {
                    "30".to_string()
                } else {
                    "7".to_string()
                },
                limit: "90".to_string(),
                unit: "days".to_string(),
            },
        ])
    }

    pub async fn audit_events(&self) -> Result<Vec<AuditEvent>, String> {
        let data = self.data.read().await;
        let mut items = data.audit_events.clone();
        items.reverse();
        Ok(items)
    }

    pub async fn audit_export(&self) -> Result<AuditExportResponse, String> {
        let items = self.audit_events().await?;
        Ok(AuditExportResponse { items })
    }

    pub async fn evaluations(&self) -> Result<EvaluationsResponse, String> {
        let data = self.data.read().await;
        let mut runs = data.evaluations.clone();
        runs.reverse();
        Ok(EvaluationsResponse { runs, queues: data.annotation_queues.clone() })
    }

    pub async fn run_evaluation(
        &self,
        request: RunEvaluationRequest,
    ) -> Result<EvaluationRun, String> {
        let mut data = self.data.write().await;
        let seed = seed_value(&request.agent, &request.dataset);
        let run = EvaluationRun {
            id: format!("eval_{}", Uuid::new_v4().simple()),
            label: request.label.unwrap_or_else(|| format!("{} regression", request.agent)),
            agent: request.agent.clone(),
            environment: request.environment.clone(),
            dataset: request.dataset.clone(),
            status: "passed".to_string(),
            score: format!("{}.{}", 90 + (seed % 9), seed % 10),
            created_at: now_string(),
            trigger: "console".to_string(),
        };
        data.evaluations.push(run.clone());
        record_audit(
            &mut data,
            "evaluation.run",
            &format!("{}:{}", request.environment, request.agent),
            "success",
        );
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(run)
    }

    pub async fn list_team_members(&self) -> Result<Vec<TeamMember>, String> {
        let data = self.data.read().await;
        Ok(data.team_members.clone())
    }

    pub async fn invite_team_member(
        &self,
        request: InviteTeamMemberRequest,
    ) -> Result<TeamMember, String> {
        let mut data = self.data.write().await;
        let member = TeamMember {
            id: format!("tm_{}", Uuid::new_v4().simple()),
            name: request.name.unwrap_or_else(|| {
                request.email.split('@').next().unwrap_or("new member").replace('.', " ")
            }),
            email: request.email.clone(),
            role: request.role.clone(),
            last_active: "invited just now".to_string(),
            invitation_status: "pending".to_string(),
        };
        data.team_members.push(member.clone());
        record_audit(&mut data, "team.invite", &request.email, "success");
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(member)
    }

    pub async fn remove_team_member(&self, member_id: &str) -> Result<(), String> {
        let mut data = self.data.write().await;
        let member = data
            .team_members
            .iter()
            .find(|member| member.id == member_id)
            .cloned()
            .ok_or_else(|| format!("team member not found: {member_id}"))?;
        data.team_members.retain(|member| member.id != member_id);
        record_audit(&mut data, "team.remove", &member.email, "success");
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn catalog(&self) -> Result<Vec<CatalogTemplate>, String> {
        let data = self.data.read().await;
        Ok(data.catalog_templates.clone())
    }

    pub async fn deploy_template(
        &self,
        template_id: &str,
        environment: &str,
        workspace_id: Option<String>,
    ) -> Result<DeploymentRecord, String> {
        let template = {
            let data = self.data.read().await;
            data.catalog_templates
                .iter()
                .find(|item| item.id == template_id)
                .cloned()
                .ok_or_else(|| format!("template not found: {template_id}"))?
        };
        let manifest = template_manifest(&template);
        let record = self
            .push_deployment(PushDeploymentRequest {
                workspace_id,
                environment: environment.to_string(),
                manifest,
                bundle_path: format!("catalog://{template_id}"),
                checksum_sha256: format!("{:x}", Sha256::digest(template_id.as_bytes())),
                binary_path: None,
            })
            .await?;
        Ok(record)
    }

    pub async fn api_explorer(&self, base_url: &str) -> Result<ApiExplorerResponse, String> {
        let data = self.data.read().await;
        Ok(ApiExplorerResponse {
            endpoints: endpoint_docs(base_url),
            api_keys: data.api_keys.iter().map(StoredApiKey::summary).collect(),
            openapi_url: format!("{base_url}/openapi.json"),
        })
    }

    pub async fn create_api_key(
        &self,
        request: CreateApiKeyRequest,
    ) -> Result<CreateApiKeyResponse, String> {
        let workspace_id = self.default_workspace_id().await?;
        self.create_api_key_for_workspace(&workspace_id, request).await
    }

    pub async fn create_api_key_for_workspace(
        &self,
        workspace_id: &str,
        request: CreateApiKeyRequest,
    ) -> Result<CreateApiKeyResponse, String> {
        let mut data = self.data.write().await;
        let token = format!("adkpk_{}", Uuid::new_v4().simple());
        let api_key = StoredApiKey {
            id: format!("key_{}", Uuid::new_v4().simple()),
            name: request.name.clone(),
            preview: format!("••••{}", &token[token.len().saturating_sub(4)..]),
            workspace_id: workspace_id.to_string(),
            scopes: if request.scopes.is_empty() {
                vec!["deploy:read".to_string(), "deploy:write".to_string()]
            } else {
                request.scopes.clone()
            },
            last_used: "never".to_string(),
            secret_hash: hash_secret(&token),
        };
        let summary = api_key.summary();
        data.api_keys.push(api_key);
        record_audit(&mut data, "api-key.create", &request.name, "success");
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(CreateApiKeyResponse { api_key: summary, token })
    }

    pub async fn delete_api_key(&self, key_id: &str) -> Result<(), String> {
        let mut data = self.data.write().await;
        let key = data
            .api_keys
            .iter()
            .find(|key| key.id == key_id)
            .cloned()
            .ok_or_else(|| format!("api key not found: {key_id}"))?;
        data.api_keys.retain(|key| key.id != key_id);
        record_audit(&mut data, "api-key.delete", &key.name, "success");
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn alert_rules(&self) -> Result<Vec<AlertRule>, String> {
        let data = self.data.read().await;
        Ok(data.alert_rules.clone())
    }

    pub async fn create_alert_rule(
        &self,
        request: CreateAlertRuleRequest,
    ) -> Result<AlertRule, String> {
        let mut data = self.data.write().await;
        let rule = AlertRule {
            id: format!("rule_{}", Uuid::new_v4().simple()),
            name: request.name.clone(),
            condition: request.condition.clone(),
            channel: request.channel.clone(),
            status: "active".to_string(),
            suppressed_until: None,
        };
        data.alert_rules.push(rule.clone());
        data.alert_history.push(AlertEvent {
            id: format!("alert_{}", Uuid::new_v4().simple()),
            rule_name: rule.name.clone(),
            state: "configured".to_string(),
            triggered_at: now_string(),
            detail: rule.condition.clone(),
        });
        record_audit(&mut data, "alert-rule.create", &rule.name, "success");
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(rule)
    }

    pub async fn alert_history(&self) -> Result<Vec<AlertEvent>, String> {
        let data = self.data.read().await;
        let mut history = data.alert_history.clone();
        history.reverse();
        Ok(history)
    }

    pub async fn suppress_alert_rule(&self, rule_id: &str) -> Result<AlertRule, String> {
        let mut data = self.data.write().await;
        let rule = data
            .alert_rules
            .iter_mut()
            .find(|rule| rule.id == rule_id)
            .ok_or_else(|| format!("alert rule not found: {rule_id}"))?;
        rule.status = "suppressed".to_string();
        rule.suppressed_until = Some("for 2 hours".to_string());
        let rule = rule.clone();
        data.alert_history.push(AlertEvent {
            id: format!("alert_{}", Uuid::new_v4().simple()),
            rule_name: rule.name.clone(),
            state: "suppressed".to_string(),
            triggered_at: now_string(),
            detail: "maintenance window".to_string(),
        });
        record_audit(&mut data, "alert-rule.suppress", &rule.name, "success");
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(rule)
    }

    pub async fn create_environment(
        &self,
        request: CreateEnvironmentRequest,
    ) -> Result<EnvironmentSummary, String> {
        let mut data = self.data.write().await;
        ensure_environment(&mut data, &request.name, &request.region);
        record_audit(&mut data, "environment.create", &request.name, "success");
        let environment = current_environments(&data)
            .into_iter()
            .find(|item| item.name == request.name)
            .ok_or_else(|| "environment creation failed".to_string())?;
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(environment)
    }

    pub async fn promote_environment(
        &self,
        target_environment: &str,
        request: PromoteEnvironmentRequest,
    ) -> Result<DeploymentRecord, String> {
        let source = {
            let data = self.data.read().await;
            latest_deployment(&data, None, &request.source_environment, Some(&request.agent_name))
                .ok_or_else(|| "source deployment not found".to_string())?
        };
        let record = self
            .push_deployment(PushDeploymentRequest {
                workspace_id: request.workspace_id,
                environment: target_environment.to_string(),
                manifest: source.manifest,
                bundle_path: format!(
                    "promotion://{}/{}",
                    request.source_environment, request.agent_name
                ),
                checksum_sha256: source.checksum_sha256,
                binary_path: None,
            })
            .await?;
        Ok(record)
    }

    pub async fn change_tier(
        &self,
        request: ChangeTierRequest,
    ) -> Result<WorkspaceResponse, String> {
        let mut data = self.data.write().await;
        let workspace =
            data.workspaces.first_mut().ok_or_else(|| "workspace not found".to_string())?;
        workspace.plan = request.tier.clone();
        let workspace = workspace.clone();
        record_audit(&mut data, "billing.tier.change", &workspace.name, &request.tier);
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(WorkspaceResponse { workspace })
    }

    pub async fn openapi_spec(&self, base_url: &str) -> Result<serde_json::Value, String> {
        Ok(json!({
            "openapi": "3.1.0",
            "info": {
                "title": "ADK Deployment Platform API",
                "version": "0.4.0"
            },
            "servers": [{ "url": base_url }],
            "paths": endpoint_docs(base_url)
                .into_iter()
                .fold(serde_json::Map::new(), |mut acc, endpoint| {
                    acc.insert(endpoint.path.clone(), json!({
                        endpoint.method.to_lowercase(): {
                            "summary": endpoint.description,
                            "security": [{ "bearerAuth": [] }]
                        }
                    }));
                    acc
                }),
            "components": {
                "securitySchemes": {
                    "bearerAuth": {
                        "type": "http",
                        "scheme": "bearer"
                    }
                }
            }
        }))
    }

    pub async fn approve_hitl(
        &self,
        checkpoint_id: &str,
        request: HitlDecisionRequest,
    ) -> Result<HitlCheckpoint, String> {
        self.update_hitl_state(checkpoint_id, request, "Approved").await
    }

    pub async fn reject_hitl(
        &self,
        checkpoint_id: &str,
        request: HitlDecisionRequest,
    ) -> Result<HitlCheckpoint, String> {
        self.update_hitl_state(checkpoint_id, request, "Rejected").await
    }

    async fn update_hitl_state(
        &self,
        checkpoint_id: &str,
        request: HitlDecisionRequest,
        state_value: &str,
    ) -> Result<HitlCheckpoint, String> {
        let mut data = self.data.write().await;
        let item = data
            .hitl_items
            .iter_mut()
            .find(|item| item.id == checkpoint_id)
            .ok_or_else(|| format!("checkpoint not found: {checkpoint_id}"))?;
        item.state = state_value.to_string();
        if let Some(reviewer) = request.reviewer {
            item.wait = format!("handled by {reviewer}");
        }
        let item = item.clone();
        record_audit(
            &mut data,
            "hitl.decision",
            &format!("{}:{}", item.agent, item.id),
            state_value,
        );
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(item)
    }

    pub async fn restart_agent(
        &self,
        environment: &str,
        agent_name: &str,
    ) -> Result<AgentActionResponse, String> {
        let mut data = self.data.write().await;
        let deployment = latest_deployment_mut(&mut data, environment, agent_name)
            .ok_or_else(|| format!("agent not found: {agent_name}"))?;
        deployment.rollout_phase = "restarted".to_string();
        let deployment = deployment.clone();
        record_audit(&mut data, "agent.restart", &format!("{environment}:{agent_name}"), "success");
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(AgentActionResponse {
            message: format!("Restarted {agent_name} in {environment}."),
            deployment,
        })
    }

    pub async fn scale_agent(
        &self,
        environment: &str,
        agent_name: &str,
        request: AgentScaleRequest,
    ) -> Result<AgentActionResponse, String> {
        let mut data = self.data.write().await;
        let deployment = latest_deployment_mut(&mut data, environment, agent_name)
            .ok_or_else(|| format!("agent not found: {agent_name}"))?;
        deployment.manifest.scaling.min_instances = request.min_instances;
        deployment.manifest.scaling.max_instances = request.max_instances;
        deployment.rollout_phase = "scaled_override".to_string();
        let deployment = deployment.clone();
        record_audit(
            &mut data,
            "agent.scale",
            &format!("{environment}:{agent_name}"),
            &format!("{}-{}", request.min_instances, request.max_instances),
        );
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(AgentActionResponse {
            message: format!(
                "Scaled {agent_name} in {environment} to {}-{} instances.",
                request.min_instances, request.max_instances
            ),
            deployment,
        })
    }

    pub async fn agent_detail(
        &self,
        agent_name: &str,
        environment: &str,
    ) -> Result<AgentDetail, String> {
        let workspace_id = self.default_workspace_id().await?;
        self.agent_detail_for_workspace(&workspace_id, agent_name, environment).await
    }

    pub async fn agent_detail_for_workspace(
        &self,
        workspace_id: &str,
        agent_name: &str,
        environment: &str,
    ) -> Result<AgentDetail, String> {
        let data = self.data.read().await;
        let deployment =
            latest_deployment(&data, Some(workspace_id), environment, Some(agent_name))
                .ok_or_else(|| format!("agent not found: {agent_name}"))?;
        let request_seed = seed_value(&deployment.agent_name, &deployment.version);
        Ok(AgentDetail {
            name: deployment.agent_name.clone(),
            environment: deployment.environment.clone(),
            description: deployment
                .manifest
                .agent
                .description
                .clone()
                .unwrap_or_else(|| "Deployed ADK-Rust agent".to_string()),
            endpoint: deployment.endpoint_url.clone(),
            strategy: deployment.strategy,
            scaling_policy: format!(
                "min {} · max {} · p95 < {}ms",
                deployment.manifest.scaling.min_instances,
                deployment.manifest.scaling.max_instances,
                deployment.manifest.scaling.target_latency_ms.unwrap_or(500)
            ),
            deployment_source: deployment
                .source_reference
                .clone()
                .unwrap_or_else(|| deployment.source_kind.replace('_', " ").to_uppercase()),
            uptime: "99.95% · 30d".to_string(),
            error_rate: format!("{:.1}%", ((request_seed % 30) + 8) as f32 / 10.0),
            metrics: synthetic_metric_points(request_seed),
            instances: synthetic_instances(&deployment),
            deployments: deployment_summaries(
                &data,
                &deployment.agent_name,
                &deployment.environment,
            ),
        })
    }

    pub async fn push_deployment(
        &self,
        request: PushDeploymentRequest,
    ) -> Result<DeploymentRecord, String> {
        let mut data = self.data.write().await;
        let workspace = request
            .workspace_id
            .clone()
            .and_then(|workspace_id| {
                data.workspaces.iter().find(|item| item.id == workspace_id).cloned()
            })
            .unwrap_or_else(|| {
                data.workspaces
                    .first()
                    .cloned()
                    .unwrap_or_else(|| ensure_workspace(&mut data, "Default Workspace"))
            });

        ensure_environment(&mut data, &request.environment, &workspace.region);

        let next_version =
            deployment_version(&data, &request.manifest.agent.name, &request.environment);
        let endpoint = format!(
            "https://{}.{}.local.adk",
            request.manifest.agent.name.replace('_', "-"),
            request.environment
        );
        let rollout_phase = match request.manifest.strategy.kind {
            adk_deploy::DeploymentStrategyKind::Rolling => "rollout_complete",
            adk_deploy::DeploymentStrategyKind::BlueGreen => "green_ready",
            adk_deploy::DeploymentStrategyKind::Canary => "canary_active",
        };
        let record = DeploymentRecord {
            id: Uuid::new_v4().to_string(),
            workspace_id: workspace.id.clone(),
            environment: request.environment.clone(),
            agent_name: request.manifest.agent.name.clone(),
            version: format!("v{next_version}"),
            status: DeploymentStatusValue::Healthy,
            strategy: request.manifest.strategy.kind,
            rollout_phase: rollout_phase.to_string(),
            endpoint_url: endpoint,
            checksum_sha256: request.checksum_sha256,
            source_kind: request
                .manifest
                .source
                .as_ref()
                .map(|source| source.kind.clone())
                .unwrap_or_else(|| "cli".to_string()),
            source_reference: request.manifest.source.as_ref().and_then(|source| {
                source
                    .project_id
                    .as_ref()
                    .map(|project_id| format!("{} · {}", source.kind, project_id))
            }),
            created_at: now_string(),
            manifest: request.manifest,
        };
        data.audit_events.push(AuditEvent {
            timestamp: now_string(),
            action: "deployment.push".to_string(),
            resource: format!("{}:{}", record.environment, record.agent_name),
            result: "success".to_string(),
        });
        data.deployments.push(record.clone());
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(record)
    }

    pub async fn push_uploaded_deployment(
        &self,
        request: PushDeploymentRequest,
        bundle_name: Option<&str>,
        bundle_bytes: &[u8],
    ) -> Result<DeploymentRecord, String> {
        if bundle_bytes.is_empty() {
            return Err("uploaded bundle is empty".to_string());
        }
        request.manifest.validate().map_err(|error| error.to_string())?;
        let actual_checksum = sha256_hex(bundle_bytes);
        if actual_checksum != request.checksum_sha256 {
            return Err(format!(
                "bundle checksum mismatch: expected {}, got {actual_checksum}",
                request.checksum_sha256
            ));
        }

        let mut data = self.data.write().await;
        let workspace = request
            .workspace_id
            .clone()
            .and_then(|workspace_id| {
                data.workspaces.iter().find(|item| item.id == workspace_id).cloned()
            })
            .unwrap_or_else(|| {
                data.workspaces
                    .first()
                    .cloned()
                    .unwrap_or_else(|| ensure_workspace(&mut data, "Default Workspace"))
            });

        ensure_environment(&mut data, &request.environment, &workspace.region);

        let deployment_id = Uuid::new_v4().to_string();
        let artifact = self
            .store_uploaded_artifact(
                &deployment_id,
                &workspace.id,
                &request.manifest.agent.name,
                bundle_name,
                bundle_bytes,
                &actual_checksum,
            )
            .map_err(|error| error.to_string())?;
        let next_version =
            deployment_version(&data, &request.manifest.agent.name, &request.environment);
        let endpoint = format!(
            "https://{}.{}.local.adk",
            request.manifest.agent.name.replace('_', "-"),
            request.environment
        );
        let rollout_phase = match request.manifest.strategy.kind {
            adk_deploy::DeploymentStrategyKind::Rolling => "rollout_complete",
            adk_deploy::DeploymentStrategyKind::BlueGreen => "green_ready",
            adk_deploy::DeploymentStrategyKind::Canary => "canary_active",
        };
        let record = DeploymentRecord {
            id: deployment_id.clone(),
            workspace_id: workspace.id.clone(),
            environment: request.environment.clone(),
            agent_name: request.manifest.agent.name.clone(),
            version: format!("v{next_version}"),
            status: DeploymentStatusValue::Healthy,
            strategy: request.manifest.strategy.kind,
            rollout_phase: rollout_phase.to_string(),
            endpoint_url: endpoint,
            checksum_sha256: actual_checksum,
            source_kind: request
                .manifest
                .source
                .as_ref()
                .map(|source| source.kind.clone())
                .unwrap_or_else(|| "cli".to_string()),
            source_reference: request.manifest.source.as_ref().and_then(|source| {
                source
                    .project_id
                    .as_ref()
                    .map(|project_id| format!("{} · {}", source.kind, project_id))
            }),
            created_at: now_string(),
            manifest: request.manifest,
        };
        data.artifacts.push(artifact);
        data.audit_events.push(AuditEvent {
            timestamp: now_string(),
            action: "deployment.push".to_string(),
            resource: format!("{}:{}", record.environment, record.agent_name),
            result: "success".to_string(),
        });
        data.deployments.push(record.clone());
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(record)
    }

    pub async fn deployment_status(
        &self,
        environment: &str,
        agent_name: Option<&str>,
    ) -> Result<DeploymentStatusResponse, String> {
        let workspace_id = self.default_workspace_id().await?;
        self.deployment_status_for_workspace(&workspace_id, environment, agent_name).await
    }

    pub async fn deployment_status_for_workspace(
        &self,
        workspace_id: &str,
        environment: &str,
        agent_name: Option<&str>,
    ) -> Result<DeploymentStatusResponse, String> {
        let data = self.data.read().await;
        let deployment = latest_deployment(&data, Some(workspace_id), environment, agent_name)
            .ok_or_else(|| "deployment not found".to_string())?;
        Ok(DeploymentStatusResponse {
            metrics: synthetic_metrics(seed_value(&deployment.agent_name, &deployment.version)),
            deployment,
        })
    }

    pub async fn deployment_history(
        &self,
        environment: &str,
        agent_name: Option<&str>,
    ) -> Result<DeploymentHistoryResponse, String> {
        let workspace_id = self.default_workspace_id().await?;
        self.deployment_history_for_workspace(&workspace_id, environment, agent_name).await
    }

    pub async fn deployment_history_for_workspace(
        &self,
        workspace_id: &str,
        environment: &str,
        agent_name: Option<&str>,
    ) -> Result<DeploymentHistoryResponse, String> {
        let data = self.data.read().await;
        let mut items: Vec<_> = data
            .deployments
            .iter()
            .filter(|deployment| deployment.workspace_id == workspace_id)
            .filter(|deployment| deployment.environment == environment)
            .filter(|deployment| agent_name.is_none_or(|name| deployment.agent_name == name))
            .cloned()
            .collect();
        items.reverse();
        items.truncate(5);
        Ok(DeploymentHistoryResponse { items })
    }

    pub async fn rollback(&self, deployment_id: &str) -> Result<DeploymentStatusResponse, String> {
        let mut data = self.data.write().await;
        let target = data
            .deployments
            .iter()
            .find(|deployment| deployment.id == deployment_id)
            .cloned()
            .ok_or_else(|| format!("deployment not found: {deployment_id}"))?;

        let previous = data
            .deployments
            .iter()
            .rev()
            .find(|deployment| {
                deployment.environment == target.environment
                    && deployment.agent_name == target.agent_name
                    && deployment.id != target.id
            })
            .cloned()
            .ok_or_else(|| "rollback target not found".to_string())?;

        let mut record = previous.clone();
        let next_version = deployment_version(&data, &target.agent_name, &target.environment);
        record.id = Uuid::new_v4().to_string();
        record.version = format!("v{next_version}");
        record.status = DeploymentStatusValue::RolledBack;
        record.rollout_phase = format!("rolled_back_from_{}", target.version);
        record.created_at = now_string();
        data.audit_events.push(AuditEvent {
            timestamp: now_string(),
            action: "deployment.rollback".to_string(),
            resource: format!("{}:{}", target.environment, target.agent_name),
            result: "success".to_string(),
        });
        data.deployments.push(record.clone());
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(DeploymentStatusResponse {
            metrics: synthetic_metrics(seed_value(&record.agent_name, &record.version)),
            deployment: record,
        })
    }

    pub async fn promote(&self, deployment_id: &str) -> Result<DeploymentStatusResponse, String> {
        let mut data = self.data.write().await;
        let deployment = data
            .deployments
            .iter_mut()
            .find(|deployment| deployment.id == deployment_id)
            .ok_or_else(|| format!("deployment not found: {deployment_id}"))?;
        let resource = format!("{}:{}", deployment.environment, deployment.agent_name);
        deployment.rollout_phase = "promoted".to_string();
        deployment.status = DeploymentStatusValue::Healthy;
        let deployment = deployment.clone();
        data.audit_events.push(AuditEvent {
            timestamp: now_string(),
            action: "deployment.promote".to_string(),
            resource,
            result: "success".to_string(),
        });
        let response = DeploymentStatusResponse {
            metrics: synthetic_metrics(seed_value(&deployment.agent_name, &deployment.version)),
            deployment,
        };
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(response)
    }

    pub async fn set_secret(&self, request: SecretSetRequest) -> Result<(), String> {
        let mut data = self.data.write().await;
        let ciphertext = self.cipher.encrypt(&request.value)?;
        data.secrets.retain(|secret| {
            !(secret.environment == request.environment && secret.key == request.key)
        });
        data.secrets.push(StoredSecret {
            environment: request.environment.clone(),
            key: request.key.clone(),
            ciphertext,
            updated_at: now_string(),
        });
        data.audit_events.push(AuditEvent {
            timestamp: now_string(),
            action: "secret.set".to_string(),
            resource: format!("{}:{}", request.environment, request.key),
            result: "success".to_string(),
        });
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn list_secrets(&self, environment: &str) -> Result<SecretListResponse, String> {
        let data = self.data.read().await;
        let keys = data
            .secrets
            .iter()
            .filter(|secret| secret.environment == environment)
            .map(|secret| secret.key.clone())
            .collect();
        Ok(SecretListResponse { environment: environment.to_string(), keys })
    }

    pub async fn delete_secret(&self, environment: &str, key: &str) -> Result<(), String> {
        let mut data = self.data.write().await;
        data.secrets.retain(|secret| !(secret.environment == environment && secret.key == key));
        data.audit_events.push(AuditEvent {
            timestamp: now_string(),
            action: "secret.delete".to_string(),
            resource: format!("{environment}:{key}"),
            result: "success".to_string(),
        });
        drop(data);
        self.persist().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn persist(&self) -> Result<(), StateError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = self.data.read().await;
        let payload = serde_json::to_string_pretty(&*data)?;
        fs::write(&self.path, payload)?;
        Ok(())
    }

    fn store_uploaded_artifact(
        &self,
        deployment_id: &str,
        workspace_id: &str,
        agent_name: &str,
        bundle_name: Option<&str>,
        bundle_bytes: &[u8],
        checksum_sha256: &str,
    ) -> Result<StoredArtifact, StateError> {
        let base_dir = self
            .path
            .parent()
            .ok_or_else(|| StateError::Crypto("missing platform data directory".to_string()))?;
        let file_name = sanitized_file_name(bundle_name);
        let artifact_dir = base_dir
            .join("artifacts")
            .join(safe_path_segment(workspace_id))
            .join(safe_path_segment(agent_name))
            .join(deployment_id);
        fs::create_dir_all(&artifact_dir)?;
        let artifact_path = artifact_dir.join(&file_name);
        fs::write(&artifact_path, bundle_bytes)?;
        Ok(StoredArtifact {
            deployment_id: deployment_id.to_string(),
            workspace_id: workspace_id.to_string(),
            checksum_sha256: checksum_sha256.to_string(),
            file_name,
            stored_path: artifact_path.to_string_lossy().to_string(),
            size_bytes: bundle_bytes.len() as u64,
            created_at: now_string(),
        })
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
struct PersistedState {
    workspaces: Vec<WorkspaceSummary>,
    environments: Vec<StoredEnvironment>,
    deployments: Vec<DeploymentRecord>,
    artifacts: Vec<StoredArtifact>,
    #[serde(default, deserialize_with = "deserialize_secrets")]
    secrets: Vec<StoredSecret>,
    audit_events: Vec<AuditEvent>,
    sessions: HashMap<String, String>,
    evaluations: Vec<EvaluationRun>,
    annotation_queues: Vec<AnnotationQueue>,
    team_members: Vec<TeamMember>,
    api_keys: Vec<StoredApiKey>,
    catalog_templates: Vec<CatalogTemplate>,
    alert_rules: Vec<AlertRule>,
    alert_history: Vec<AlertEvent>,
    hitl_items: Vec<HitlCheckpoint>,
}

impl PersistedState {
    fn default_seeded() -> Self {
        let workspace = WorkspaceSummary {
            id: "ws_default".to_string(),
            name: "Default Workspace".to_string(),
            plan: "Pro".to_string(),
            region: "US".to_string(),
        };
        let mut state = Self {
            workspaces: vec![workspace.clone()],
            environments: vec![
                StoredEnvironment::new("dev", "US"),
                StoredEnvironment::new("staging", "EU"),
                StoredEnvironment::new("production", "US"),
            ],
            deployments: Vec::new(),
            artifacts: Vec::new(),
            secrets: Vec::new(),
            audit_events: Vec::new(),
            sessions: HashMap::new(),
            evaluations: Vec::new(),
            annotation_queues: vec![
                AnnotationQueue {
                    id: "queue_safety".to_string(),
                    name: "Safety Review".to_string(),
                    pending_items: 3,
                    reviewers: vec!["Priya Sharma".to_string(), "Amina Njoroge".to_string()],
                    rule: "High-risk outputs require human sign-off".to_string(),
                },
                AnnotationQueue {
                    id: "queue_hiring".to_string(),
                    name: "Hiring Compliance".to_string(),
                    pending_items: 1,
                    reviewers: vec!["Jordan Lee".to_string()],
                    rule: "Candidate-facing drafts need compliance review".to_string(),
                },
            ],
            team_members: vec![
                TeamMember {
                    id: "tm_admin".to_string(),
                    name: "Amina Njoroge".to_string(),
                    email: "amina@zavora.ai".to_string(),
                    role: "platform_admin".to_string(),
                    last_active: "2m ago".to_string(),
                    invitation_status: "active".to_string(),
                },
                TeamMember {
                    id: "tm_runtime".to_string(),
                    name: "Jordan Lee".to_string(),
                    email: "jordan@zavora.ai".to_string(),
                    role: "runtime_engineer".to_string(),
                    last_active: "9m ago".to_string(),
                    invitation_status: "active".to_string(),
                },
                TeamMember {
                    id: "tm_safety".to_string(),
                    name: "Priya Sharma".to_string(),
                    email: "priya@zavora.ai".to_string(),
                    role: "safety_reviewer".to_string(),
                    last_active: "14m ago".to_string(),
                    invitation_status: "active".to_string(),
                },
            ],
            api_keys: Vec::new(),
            catalog_templates: vec![
                CatalogTemplate {
                    id: "support-copilot".to_string(),
                    name: "Support Copilot".to_string(),
                    summary: "Customer support assistant with escalation guardrails.".to_string(),
                    strategy: adk_deploy::DeploymentStrategyKind::Rolling,
                    recommended_environment: "staging".to_string(),
                    source: "catalog".to_string(),
                },
                CatalogTemplate {
                    id: "document-reviewer".to_string(),
                    name: "Document Reviewer".to_string(),
                    summary: "Policy and contract analysis with HITL checkpoints.".to_string(),
                    strategy: adk_deploy::DeploymentStrategyKind::BlueGreen,
                    recommended_environment: "production".to_string(),
                    source: "catalog".to_string(),
                },
                CatalogTemplate {
                    id: "lead-qualifier".to_string(),
                    name: "Lead Qualifier".to_string(),
                    summary: "Inbound lead triage agent with canary-first rollout.".to_string(),
                    strategy: adk_deploy::DeploymentStrategyKind::Canary,
                    recommended_environment: "production".to_string(),
                    source: "catalog".to_string(),
                },
            ],
            alert_rules: vec![
                AlertRule {
                    id: "rule_latency".to_string(),
                    name: "Latency p95 > 900ms".to_string(),
                    condition: "p95 latency above 900ms for 10m".to_string(),
                    channel: "pagerduty".to_string(),
                    status: "active".to_string(),
                    suppressed_until: None,
                },
                AlertRule {
                    id: "rule_hitl".to_string(),
                    name: "HITL backlog > 2".to_string(),
                    condition: "pending HITL checkpoints above 2".to_string(),
                    channel: "slack".to_string(),
                    status: "active".to_string(),
                    suppressed_until: None,
                },
            ],
            alert_history: vec![
                AlertEvent {
                    id: "alert_001".to_string(),
                    rule_name: "Latency p95 > 900ms".to_string(),
                    state: "firing".to_string(),
                    triggered_at: now_string(),
                    detail: "production canary crossed 942ms".to_string(),
                },
                AlertEvent {
                    id: "alert_002".to_string(),
                    rule_name: "HITL backlog > 2".to_string(),
                    state: "resolved".to_string(),
                    triggered_at: now_string(),
                    detail: "review queue drained below threshold".to_string(),
                },
            ],
            hitl_items: Vec::new(),
        };

        for environment in ["staging", "production"] {
            let strategy = if environment == "production" {
                adk_deploy::DeploymentStrategyKind::Canary
            } else {
                adk_deploy::DeploymentStrategyKind::Rolling
            };
            let manifest = demo_manifest(
                if environment == "production" {
                    "job-hunter-super-agent"
                } else {
                    "newsletter-agent"
                },
                strategy,
            );
            state.deployments.push(DeploymentRecord {
                id: Uuid::new_v4().to_string(),
                workspace_id: workspace.id.clone(),
                environment: environment.to_string(),
                agent_name: manifest.agent.name.clone(),
                version: if environment == "production" { "v42" } else { "v18" }.to_string(),
                status: if environment == "production" {
                    DeploymentStatusValue::Healthy
                } else {
                    DeploymentStatusValue::Degraded
                },
                strategy,
                rollout_phase: if environment == "production" {
                    "canary_active".to_string()
                } else {
                    "rollout_complete".to_string()
                },
                endpoint_url: format!("https://{}.{}.local.adk", manifest.agent.name, environment),
                checksum_sha256: format!("{:x}", Sha256::digest(manifest.agent.name.as_bytes())),
                source_kind: "adk_studio".to_string(),
                source_reference: Some(format!(
                    "adk_studio · project_{}",
                    &manifest.agent.name[..3]
                )),
                created_at: now_string(),
                manifest,
            });
        }
        state.evaluations = state
            .deployments
            .iter()
            .map(|deployment| EvaluationRun {
                id: format!("eval_{}", deployment.id.replace('-', "")),
                label: format!("{} regression", deployment.agent_name),
                agent: deployment.agent_name.clone(),
                environment: deployment.environment.clone(),
                dataset: format!("{}-golden", deployment.environment),
                status: if deployment.environment == "production" {
                    "passed".to_string()
                } else {
                    "warning".to_string()
                },
                score: if deployment.environment == "production" {
                    "97.8".to_string()
                } else {
                    "92.4".to_string()
                },
                created_at: deployment.created_at.clone(),
                trigger: "seed".to_string(),
            })
            .collect();
        state.hitl_items = state
            .deployments
            .iter()
            .filter(|deployment| deployment.environment == "production")
            .enumerate()
            .map(|(index, deployment)| HitlCheckpoint {
                id: format!("cp_{}", 1800 + index),
                agent: deployment.agent_name.clone(),
                wait: format!("{}m", 5 + index * 4),
                checkpoint_type: if index == 0 {
                    "Application approval".to_string()
                } else {
                    "Content approval".to_string()
                },
                state: "Pending".to_string(),
            })
            .collect();
        state
    }

    fn hydrate_missing(&mut self) {
        if self.workspaces.is_empty() {
            self.workspaces.push(WorkspaceSummary {
                id: "ws_default".to_string(),
                name: "Default Workspace".to_string(),
                plan: "Pro".to_string(),
                region: "US".to_string(),
            });
        }
        if self.environments.is_empty() {
            self.environments = vec![
                StoredEnvironment::new("dev", "US"),
                StoredEnvironment::new("staging", "EU"),
                StoredEnvironment::new("production", "US"),
            ];
        }
        if self.team_members.is_empty() {
            self.team_members = PersistedState::default_seeded().team_members;
        }
        if self.annotation_queues.is_empty() {
            self.annotation_queues = PersistedState::default_seeded().annotation_queues;
        }
        if self.catalog_templates.is_empty() {
            self.catalog_templates = PersistedState::default_seeded().catalog_templates;
        }
        if self.alert_rules.is_empty() {
            self.alert_rules = PersistedState::default_seeded().alert_rules;
        }
        if self.alert_history.is_empty() {
            self.alert_history = PersistedState::default_seeded().alert_history;
        }
        if self.evaluations.is_empty() {
            self.evaluations = self
                .deployments
                .iter()
                .map(|deployment| EvaluationRun {
                    id: format!("eval_{}", deployment.id.replace('-', "")),
                    label: format!("{} regression", deployment.agent_name),
                    agent: deployment.agent_name.clone(),
                    environment: deployment.environment.clone(),
                    dataset: format!("{}-golden", deployment.environment),
                    status: "passed".to_string(),
                    score: "96.0".to_string(),
                    created_at: deployment.created_at.clone(),
                    trigger: "migration".to_string(),
                })
                .collect();
        }
        if self.hitl_items.is_empty() {
            self.hitl_items = self
                .deployments
                .iter()
                .filter(|deployment| deployment.environment == "production")
                .take(2)
                .enumerate()
                .map(|(index, deployment)| HitlCheckpoint {
                    id: format!("cp_{}", 1800 + index),
                    agent: deployment.agent_name.clone(),
                    wait: format!("{}m", 5 + index * 4),
                    checkpoint_type: "Application approval".to_string(),
                    state: "Pending".to_string(),
                })
                .collect();
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredEnvironment {
    name: String,
    region: String,
}

impl StoredEnvironment {
    fn new(name: impl Into<String>, region: impl Into<String>) -> Self {
        Self { name: name.into(), region: region.into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredArtifact {
    deployment_id: String,
    workspace_id: String,
    checksum_sha256: String,
    file_name: String,
    stored_path: String,
    size_bytes: u64,
    created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSecret {
    environment: String,
    key: String,
    ciphertext: String,
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct LegacyStoredSecret {
    ciphertext: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredApiKey {
    id: String,
    name: String,
    preview: String,
    #[serde(default)]
    workspace_id: String,
    scopes: Vec<String>,
    last_used: String,
    #[serde(default, alias = "secret")]
    secret_hash: String,
}

impl StoredApiKey {
    fn summary(&self) -> ApiKeySummary {
        ApiKeySummary {
            id: self.id.clone(),
            name: self.name.clone(),
            preview: self.preview.clone(),
            scopes: self.scopes.clone(),
            last_used: self.last_used.clone(),
        }
    }

    fn matches(&self, token: &str) -> bool {
        self.secret_hash == token || self.secret_hash == hash_secret(token)
    }
}

fn deserialize_secrets<'de, D>(deserializer: D) -> Result<Vec<StoredSecret>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StoredSecretsRepr {
        List(Vec<StoredSecret>),
        Map(BTreeMap<String, LegacyStoredSecret>),
    }

    let value = Option::<StoredSecretsRepr>::deserialize(deserializer)?;
    Ok(match value {
        Some(StoredSecretsRepr::List(items)) => items,
        Some(StoredSecretsRepr::Map(items)) => items
            .into_iter()
            .map(|(compound_key, secret)| {
                let (environment, key) = compound_key
                    .split_once("::")
                    .map(|(environment, key)| (environment.to_string(), key.to_string()))
                    .unwrap_or_else(|| ("legacy".to_string(), compound_key));
                StoredSecret {
                    environment,
                    key,
                    ciphertext: secret.ciphertext,
                    updated_at: secret.updated_at,
                }
            })
            .collect(),
        None => Vec::new(),
    })
}

fn hash_secret(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
}

fn sha256_hex(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

#[derive(Debug)]
struct SecretCipher {
    key: [u8; 32],
}

impl SecretCipher {
    fn load_from_dir(base_dir: &std::path::Path) -> Result<Self, StateError> {
        let path = base_dir.join("master.key");
        if path.exists() {
            let raw = fs::read_to_string(path)?;
            let bytes = STANDARD
                .decode(raw.trim())
                .map_err(|error| StateError::Crypto(error.to_string()))?;
            let mut key = [0_u8; 32];
            if bytes.len() != 32 {
                return Err(StateError::Crypto("invalid key length".to_string()));
            }
            key.copy_from_slice(&bytes);
            Ok(Self { key })
        } else {
            let mut key = [0_u8; 32];
            rand::thread_rng().fill_bytes(&mut key);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, STANDARD.encode(key))?;
            Ok(Self { key })
        }
    }

    fn encrypt(&self, value: &str) -> Result<String, String> {
        let cipher = Aes256Gcm::new_from_slice(&self.key).map_err(|error| error.to_string())?;
        let mut nonce_bytes = [0_u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext =
            cipher.encrypt(nonce, value.as_bytes()).map_err(|error| error.to_string())?;
        Ok(format!("{}:{}", STANDARD.encode(nonce_bytes), STANDARD.encode(ciphertext)))
    }

    #[allow(dead_code)]
    fn decrypt(&self, value: &str) -> Result<String, String> {
        let (nonce, ciphertext) =
            value.split_once(':').ok_or_else(|| "invalid ciphertext format".to_string())?;
        let nonce = STANDARD.decode(nonce).map_err(|error| error.to_string())?;
        let ciphertext = STANDARD.decode(ciphertext).map_err(|error| error.to_string())?;
        let cipher = Aes256Gcm::new_from_slice(&self.key).map_err(|error| error.to_string())?;
        let plaintext = cipher
            .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
            .map_err(|error| error.to_string())?;
        String::from_utf8(plaintext).map_err(|error| error.to_string())
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum StateError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("crypto error: {0}")]
    Crypto(String),
}

fn platform_data_dir() -> Result<PathBuf, StateError> {
    let base =
        dirs::data_local_dir().ok_or_else(|| StateError::Crypto("missing data dir".into()))?;
    Ok(base.join("adk-deploy-server"))
}

fn now_string() -> String {
    Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

fn ensure_workspace(data: &mut PersistedState, name: &str) -> WorkspaceSummary {
    if let Some(workspace) =
        data.workspaces.iter().find(|workspace| workspace.name == name).cloned()
    {
        return workspace;
    }
    let workspace = WorkspaceSummary {
        id: format!("ws_{}", Uuid::new_v4().simple()),
        name: name.to_string(),
        plan: "Pro".to_string(),
        region: "US".to_string(),
    };
    data.workspaces.push(workspace.clone());
    workspace
}

fn ensure_environment(data: &mut PersistedState, environment: &str, region: &str) {
    if data.environments.iter().any(|item| item.name == environment) {
        return;
    }
    data.environments.push(StoredEnvironment::new(environment, region));
}

fn record_audit(data: &mut PersistedState, action: &str, resource: &str, result: &str) {
    data.audit_events.push(AuditEvent {
        timestamp: now_string(),
        action: action.to_string(),
        resource: resource.to_string(),
        result: result.to_string(),
    });
}

fn sanitized_file_name(file_name: Option<&str>) -> String {
    file_name
        .and_then(|value| Path::new(value).file_name())
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "bundle.tar.gz".to_string())
}

fn safe_path_segment(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn deployment_version(data: &PersistedState, agent_name: &str, environment: &str) -> usize {
    data.deployments
        .iter()
        .filter(|deployment| {
            deployment.agent_name == agent_name && deployment.environment == environment
        })
        .count()
        + 1
}

fn latest_deployment(
    data: &PersistedState,
    workspace_id: Option<&str>,
    environment: &str,
    agent_name: Option<&str>,
) -> Option<DeploymentRecord> {
    data.deployments
        .iter()
        .rev()
        .find(|deployment| {
            workspace_id.is_none_or(|workspace_id| deployment.workspace_id == workspace_id)
                && deployment.environment == environment
                && agent_name.is_none_or(|name| deployment.agent_name == name)
        })
        .cloned()
}

fn latest_deployment_mut<'a>(
    data: &'a mut PersistedState,
    environment: &str,
    agent_name: &str,
) -> Option<&'a mut DeploymentRecord> {
    data.deployments.iter_mut().rev().find(|deployment| {
        deployment.environment == environment && deployment.agent_name == agent_name
    })
}

fn current_agent_summaries(data: &PersistedState, workspace_id: Option<&str>) -> Vec<AgentSummary> {
    let mut latest = BTreeMap::<(String, String), DeploymentRecord>::new();
    for deployment in &data.deployments {
        if workspace_id.is_some_and(|workspace_id| deployment.workspace_id != workspace_id) {
            continue;
        }
        latest.insert(
            (deployment.agent_name.clone(), deployment.environment.clone()),
            deployment.clone(),
        );
    }

    latest
        .into_values()
        .map(|deployment| {
            let seed = seed_value(&deployment.agent_name, &deployment.version);
            AgentSummary {
                name: deployment.agent_name.clone(),
                environment: deployment.environment.clone(),
                version: deployment.version.clone(),
                health: match deployment.status {
                    DeploymentStatusValue::Degraded | DeploymentStatusValue::Failed => {
                        "Degraded".to_string()
                    }
                    _ => "Healthy".to_string(),
                },
                instances: deployment.manifest.scaling.min_instances.max(1) as usize,
                request_rate: format!("{}.{}/min", (seed % 9) + 1, (seed % 7) + 1),
                latency_p95: format!("p95 {}ms", 800 + (seed % 1800)),
                deployed_at: deployment.created_at.clone(),
                source_kind: deployment.source_kind.clone(),
                source_reference: deployment.source_reference.clone(),
            }
        })
        .collect()
}

fn current_environments(data: &PersistedState) -> Vec<EnvironmentSummary> {
    data.environments
        .iter()
        .map(|environment| EnvironmentSummary {
            name: environment.name.clone(),
            agents: data
                .deployments
                .iter()
                .filter(|deployment| deployment.environment == environment.name)
                .map(|deployment| deployment.agent_name.clone())
                .collect::<BTreeSet<_>>()
                .len(),
            status: if data.deployments.iter().any(|deployment| {
                deployment.environment == environment.name
                    && deployment.status == DeploymentStatusValue::Degraded
            }) {
                "Degraded".to_string()
            } else {
                "Healthy".to_string()
            },
            region: environment.region.clone(),
        })
        .collect()
}

fn current_alert_summaries(data: &PersistedState, agents: &[AgentSummary]) -> Vec<AlertSummary> {
    let mut alerts = synthetic_alerts(agents);
    alerts.extend(data.alert_rules.iter().map(|rule| AlertSummary {
        name: rule.name.clone(),
        state: rule.status.clone(),
        description: rule.condition.clone(),
    }));
    alerts
}

fn synthetic_traces(agents: &[AgentSummary]) -> Vec<TraceSummary> {
    agents
        .iter()
        .take(3)
        .enumerate()
        .map(|(index, agent)| TraceSummary {
            id: format!("tr_{:08x}", seed_value(&agent.name, &agent.version) + index as u32),
            status: if index == 1 { "error".to_string() } else { "success".to_string() },
            model: if index % 2 == 0 {
                "gemini-2.5-pro".to_string()
            } else {
                "gpt-4.1-mini".to_string()
            },
            duration: format!("{}.{index}s", 1 + index),
            tokens: format!("{},{}", 2 + index, 300 + index * 111),
            cost: format!("$0.0{}", index + 1),
            step: format!("{} inference", agent.name),
        })
        .collect()
}

fn synthetic_logs(agents: &[AgentSummary]) -> Vec<LogEntry> {
    agents
        .iter()
        .take(4)
        .enumerate()
        .map(|(index, agent)| LogEntry {
            time: format!("12:0{}:1{}", index, index),
            level: match index {
                0 => "INFO",
                1 => "WARN",
                2 => "ERROR",
                _ => "DEBUG",
            }
            .to_string(),
            instance: format!("inst_{:04x}", seed_value(&agent.name, &agent.version)),
            message: format!("{} deployment event {}", agent.name, index + 1),
        })
        .collect()
}

fn synthetic_usage(agents: &[AgentSummary]) -> Vec<BillingSummary> {
    vec![
        BillingSummary {
            label: "Compute".to_string(),
            value: format!("${}", agents.len() * 940),
            sub: "instance-hours".to_string(),
        },
        BillingSummary {
            label: "Traces".to_string(),
            value: format!("{}k", agents.len() * 12),
            sub: "monthly traces".to_string(),
        },
        BillingSummary {
            label: "Storage".to_string(),
            value: format!("{} GB", agents.len() * 24),
            sub: "logs, datasets, artifacts".to_string(),
        },
        BillingSummary {
            label: "Egress".to_string(),
            value: format!("{} GB", agents.len() * 63),
            sub: "APIs + streaming".to_string(),
        },
    ]
}

fn synthetic_alerts(agents: &[AgentSummary]) -> Vec<AlertSummary> {
    agents
        .iter()
        .filter(|agent| agent.health == "Degraded")
        .map(|agent| AlertSummary {
            name: format!("{} degraded", agent.name),
            state: "active".to_string(),
            description: "error rate exceeded threshold".to_string(),
        })
        .collect()
}

fn synthetic_metric_points(seed: u32) -> Vec<MetricPoint> {
    (0..12)
        .map(|index| MetricPoint {
            label: format!("t{}", index + 1),
            value: 25 + ((seed + index) % 40),
        })
        .collect()
}

fn synthetic_instances(deployment: &DeploymentRecord) -> Vec<ActiveInstance> {
    let count = deployment.manifest.scaling.min_instances.max(1);
    (0..count)
        .map(|index| ActiveInstance {
            id: format!(
                "inst_{:04x}",
                seed_value(&deployment.agent_name, &format!("{}-{index}", deployment.version))
            ),
            state: if deployment.strategy == adk_deploy::DeploymentStrategyKind::Canary
                && index == count - 1
            {
                "Canary".to_string()
            } else {
                "Healthy".to_string()
            },
            stats: format!(
                "CPU {}% · Mem {}% · {} conns",
                30 + (index * 7),
                45 + (index * 6),
                80 + (index * 22)
            ),
        })
        .collect()
}

fn deployment_summaries(
    data: &PersistedState,
    agent_name: &str,
    environment: &str,
) -> Vec<DeploymentSummary> {
    data.deployments
        .iter()
        .rev()
        .filter(|deployment| {
            deployment.agent_name == agent_name && deployment.environment == environment
        })
        .take(5)
        .map(|deployment| DeploymentSummary {
            version: deployment.version.clone(),
            timestamp: deployment.created_at.clone(),
            status: format!("{:?}", deployment.status).to_lowercase(),
            strategy: deployment.strategy,
            triggered_by: deployment.source_kind.clone(),
        })
        .collect()
}

fn synthetic_metrics(seed: u32) -> MetricsSummary {
    MetricsSummary {
        request_rate: format!("{} req/min", 500 + (seed % 2500)),
        latency_p50: format!("{}ms", 140 + (seed % 120)),
        latency_p95: format!("{}ms", 750 + (seed % 400)),
        latency_p99: format!("{}ms", 1400 + (seed % 900)),
        error_rate: format!("{:.1}%", ((seed % 20) + 5) as f32 / 10.0),
        active_connections: 80 + (seed % 260) as usize,
    }
}

fn seed_value(left: &str, right: &str) -> u32 {
    left.bytes().chain(right.bytes()).fold(0_u32, |acc, byte| acc.wrapping_add(byte as u32))
}

fn endpoint_docs(base_url: &str) -> Vec<EndpointDoc> {
    let base_url = base_url.trim_end_matches('/');
    let token = "<token>";
    vec![
        EndpointDoc {
            id: "auth-session".to_string(),
            method: "GET".to_string(),
            path: "/api/v1/auth/session".to_string(),
            description:
                "Validate the current bearer token and return the resolved operator session."
                    .to_string(),
            auth: "Bearer token".to_string(),
            sample_curl: format!("curl -H 'Authorization: Bearer {token}' {base_url}/auth/session"),
        },
        EndpointDoc {
            id: "dashboard".to_string(),
            method: "GET".to_string(),
            path: "/api/v1/dashboard".to_string(),
            description: "Workspace overview, live agents, logs, traces, alerts, and HITL."
                .to_string(),
            auth: "Bearer token or API key".to_string(),
            sample_curl: format!("curl -H 'Authorization: Bearer {token}' {base_url}/dashboard"),
        },
        EndpointDoc {
            id: "deployments-status".to_string(),
            method: "GET".to_string(),
            path: "/api/v1/deployments/status".to_string(),
            description: "Latest rollout state and metrics for one environment or agent."
                .to_string(),
            auth: "Bearer token or API key".to_string(),
            sample_curl: format!(
                "curl -H 'Authorization: Bearer {token}' '{base_url}/deployments/status?environment=production&agent=job-hunter-super-agent'"
            ),
        },
        EndpointDoc {
            id: "deployments-push".to_string(),
            method: "POST".to_string(),
            path: "/api/v1/deployments".to_string(),
            description: "Push a validated bundle and manifest into the control plane.".to_string(),
            auth: "Bearer token or API key".to_string(),
            sample_curl: format!(
                "curl -X POST -H 'Authorization: Bearer {token}' -H 'Content-Type: application/json' {base_url}/deployments"
            ),
        },
        EndpointDoc {
            id: "secrets".to_string(),
            method: "POST".to_string(),
            path: "/api/v1/secrets".to_string(),
            description: "Create or rotate environment-scoped secrets.".to_string(),
            auth: "Bearer token or API key".to_string(),
            sample_curl: format!(
                "curl -X POST -H 'Authorization: Bearer {token}' -H 'Content-Type: application/json' -d '{{\"environment\":\"production\",\"key\":\"OPENAI_API_KEY\",\"value\":\"secret\"}}' {base_url}/secrets"
            ),
        },
        EndpointDoc {
            id: "evaluations".to_string(),
            method: "POST".to_string(),
            path: "/api/v1/evaluations".to_string(),
            description: "Run an evaluation suite against a deployed agent.".to_string(),
            auth: "Bearer token or API key".to_string(),
            sample_curl: format!(
                "curl -X POST -H 'Authorization: Bearer {token}' -H 'Content-Type: application/json' -d '{{\"agent\":\"job-hunter-super-agent\",\"environment\":\"production\",\"dataset\":\"production-golden\"}}' {base_url}/evaluations"
            ),
        },
        EndpointDoc {
            id: "catalog-deploy".to_string(),
            method: "POST".to_string(),
            path: "/api/v1/catalog/{templateId}/deploy".to_string(),
            description: "Deploy a catalog template into an environment.".to_string(),
            auth: "Bearer token or API key".to_string(),
            sample_curl: format!(
                "curl -X POST -H 'Authorization: Bearer {token}' -H 'Content-Type: application/json' -d '{{\"environment\":\"staging\"}}' {base_url}/catalog/support-copilot/deploy"
            ),
        },
        EndpointDoc {
            id: "alerts".to_string(),
            method: "POST".to_string(),
            path: "/api/v1/alerts/rules".to_string(),
            description: "Create an alert rule for latency, error, or backlog conditions."
                .to_string(),
            auth: "Bearer token or API key".to_string(),
            sample_curl: format!(
                "curl -X POST -H 'Authorization: Bearer {token}' -H 'Content-Type: application/json' -d '{{\"name\":\"Latency p95 > 900ms\",\"condition\":\"p95 latency above 900ms for 10m\",\"channel\":\"pagerduty\"}}' {base_url}/alerts/rules"
            ),
        },
        EndpointDoc {
            id: "hitl".to_string(),
            method: "POST".to_string(),
            path: "/api/v1/hitl/{checkpointId}/approve".to_string(),
            description: "Approve a queued human-in-the-loop checkpoint.".to_string(),
            auth: "Bearer token or API key".to_string(),
            sample_curl: format!(
                "curl -X POST -H 'Authorization: Bearer {token}' -H 'Content-Type: application/json' -d '{{\"reviewer\":\"operator@zavora.ai\"}}' {base_url}/hitl/cp_1800/approve"
            ),
        },
        EndpointDoc {
            id: "team".to_string(),
            method: "POST".to_string(),
            path: "/api/v1/team".to_string(),
            description: "Invite a new workspace operator or reviewer.".to_string(),
            auth: "Bearer token or API key".to_string(),
            sample_curl: format!(
                "curl -X POST -H 'Authorization: Bearer {token}' -H 'Content-Type: application/json' -d '{{\"email\":\"new.operator@example.com\",\"role\":\"runtime_engineer\"}}' {base_url}/team"
            ),
        },
    ]
}

fn demo_manifest(
    agent_name: &str,
    strategy: adk_deploy::DeploymentStrategyKind,
) -> adk_deploy::DeploymentManifest {
    let mut manifest = adk_deploy::DeploymentManifest::default();
    manifest.agent.name = agent_name.to_string();
    manifest.agent.binary = agent_name.to_string();
    manifest.agent.description = Some(format!("Synthetic deployment for {agent_name}"));
    manifest.strategy.kind = strategy;
    if strategy == adk_deploy::DeploymentStrategyKind::Canary {
        manifest.strategy.traffic_percent = Some(10);
    }
    manifest
}

fn template_manifest(template: &CatalogTemplate) -> adk_deploy::DeploymentManifest {
    let mut manifest = adk_deploy::DeploymentManifest::default();
    manifest.agent.name = template.id.clone();
    manifest.agent.binary = template.id.clone();
    manifest.agent.description = Some(template.summary.clone());
    manifest.strategy.kind = template.strategy;
    if template.strategy == adk_deploy::DeploymentStrategyKind::Canary {
        manifest.strategy.traffic_percent = Some(10);
    }
    manifest.source = Some(adk_deploy::SourceInfo {
        kind: template.source.clone(),
        project_id: Some(template.id.clone()),
        project_name: Some(template.name.clone()),
    });
    manifest
}

#[cfg(test)]
mod tests {
    use super::PlatformState;
    use adk_deploy::DeploymentManifest;
    use sha2::Digest;
    use tempfile::tempdir;

    #[tokio::test]
    async fn uploaded_bundles_are_persisted_and_validated() {
        let dir = tempdir().unwrap();
        let state = PlatformState::load_from_dir(dir.path().to_path_buf()).await.unwrap();
        let mut manifest = DeploymentManifest::default();
        manifest.agent.name = "artifact-agent".to_string();
        manifest.agent.binary = "artifact-agent".to_string();
        let bundle = b"artifact-bundle";
        let checksum = format!("{:x}", sha2::Sha256::digest(bundle));

        let deployment = state
            .push_uploaded_deployment(
                adk_deploy::PushDeploymentRequest {
                    workspace_id: Some("ws_default".to_string()),
                    environment: "staging".to_string(),
                    manifest,
                    bundle_path: "/tmp/artifact-agent.tar.gz".to_string(),
                    checksum_sha256: checksum.clone(),
                    binary_path: None,
                },
                Some("artifact-agent.tar.gz"),
                bundle,
            )
            .await
            .unwrap();

        let artifact_path = dir
            .path()
            .join("artifacts")
            .join("ws_default")
            .join("artifact-agent")
            .join(&deployment.id)
            .join("artifact-agent.tar.gz");
        assert_eq!(std::fs::read(&artifact_path).unwrap(), bundle);
        assert_eq!(deployment.checksum_sha256, checksum);
    }

    #[tokio::test]
    async fn uploaded_bundles_reject_bad_checksums() {
        let dir = tempdir().unwrap();
        let state = PlatformState::load_from_dir(dir.path().to_path_buf()).await.unwrap();
        let mut manifest = DeploymentManifest::default();
        manifest.agent.name = "artifact-agent".to_string();
        manifest.agent.binary = "artifact-agent".to_string();

        let result = state
            .push_uploaded_deployment(
                adk_deploy::PushDeploymentRequest {
                    workspace_id: Some("ws_default".to_string()),
                    environment: "staging".to_string(),
                    manifest,
                    bundle_path: "/tmp/artifact-agent.tar.gz".to_string(),
                    checksum_sha256: "bad-checksum".to_string(),
                    binary_path: None,
                },
                Some("artifact-agent.tar.gz"),
                b"artifact-bundle",
            )
            .await;

        assert!(result.is_err());
    }
}
