const DEFAULT_SETTINGS = {
  autoRefreshSeconds: 45,
};

const state = {
  config: null,
  session: null,
  settings: loadSettings(),
  dashboard: null,
  agents: [],
  environments: [],
  traces: [],
  logs: [],
  alerts: [],
  alertRules: [],
  alertHistory: [],
  hitl: [],
  billing: [],
  billingUsage: [],
  audit: [],
  evaluations: [],
  annotationQueues: [],
  teamMembers: [],
  catalogTemplates: [],
  apiExplorer: { endpoints: [], apiKeys: [], openapiUrl: "" },
  secrets: [],
  selectedView: "dashboard",
  selectedEnvironment: "production",
  selectedAgent: null,
  agentDetail: null,
  agentStatus: null,
  agentHistory: [],
  latestApiKeyToken: null,
  autoRefreshTimer: null,
};

const elements = {};

window.addEventListener("DOMContentLoaded", async () => {
  bindElements();
  bindEvents();
  state.config = await fetchJson("/config.json");
  scheduleAutoRefresh();
  if (state.session?.token) {
    try {
      await refreshData();
    } catch (error) {
      handleApiError(error, "Session expired. Sign in again.");
      return;
    }
  }
  render();
});

function bindElements() {
  elements.loginScreen = document.getElementById("login-screen");
  elements.loginForm = document.getElementById("login-form");
  elements.loginError = document.getElementById("login-error");
  elements.appShell = document.getElementById("app-shell");
  elements.content = document.getElementById("content");
  elements.viewTitle = document.getElementById("view-title");
  elements.workspaceLabel = document.getElementById("workspace-label");
  elements.environmentSelect = document.getElementById("environment-select");
  elements.notice = document.getElementById("notice");
  elements.navItems = Array.from(document.querySelectorAll("[data-view]"));
  elements.refreshButton = document.getElementById("refresh-button");
  elements.logoutButton = document.getElementById("logout-button");
  elements.tokenInput = document.getElementById("token-input");
}

function bindEvents() {
  elements.loginForm.addEventListener("submit", handleLogin);
  elements.refreshButton.addEventListener("click", async () => {
    await safeRefresh();
  });
  elements.logoutButton.addEventListener("click", () => {
    clearSession();
    elements.tokenInput.value = "";
    render();
    showNotice("Logged out.");
  });
  elements.environmentSelect.addEventListener("change", async (event) => {
    state.selectedEnvironment = event.target.value;
    state.selectedAgent = null;
    await safeLoadEnvironmentScopedData();
  });
  for (const item of elements.navItems) {
    item.addEventListener("click", async () => {
      state.selectedView = item.dataset.view;
      if (state.selectedView === "agents" && !state.agentDetail) {
        await safeLoadEnvironmentScopedData();
      } else {
        render();
      }
    });
  }
}

async function handleLogin(event) {
  event.preventDefault();
  hideError();
  try {
    const token = elements.tokenInput.value.trim();
    const response = await fetch(`${state.config.apiBaseUrl}/auth/session`, {
      method: "GET",
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!response.ok) {
      throw new Error("Unable to validate token.");
    }
    const authenticated = await response.json();
    state.session = {
      token,
      workspaceId: authenticated.workspaceId,
      workspaceName: authenticated.workspaceName,
      userId: authenticated.userId,
      scopes: authenticated.scopes || [],
    };
    elements.tokenInput.value = "";
    scheduleAutoRefresh();
    await safeRefresh("Connected.");
  } catch (error) {
    showLogin(error.message);
  }
}

async function safeRefresh(noticeMessage = "") {
  try {
    await refreshData();
    if (noticeMessage) {
      showNotice(noticeMessage);
    }
  } catch (error) {
    handleApiError(error, "Session expired. Sign in again.");
  }
}

async function refreshData() {
  assertSession();
  const [
    dashboard,
    agents,
    environments,
    traces,
    logs,
    alerts,
    alertRules,
    alertHistory,
    hitl,
    billing,
    billingUsage,
    audit,
    evaluations,
    catalogTemplates,
    teamMembers,
    apiExplorer,
  ] = await Promise.all([
    api("/dashboard"),
    api("/agents"),
    api("/environments"),
    api("/traces"),
    api("/logs"),
    api("/alerts"),
    api("/alerts/rules"),
    api("/alerts/history"),
    api("/hitl"),
    api("/billing"),
    api("/billing/usage"),
    api("/audit"),
    api("/evaluations"),
    api("/catalog"),
    api("/team"),
    api("/api-explorer"),
  ]);
  state.dashboard = dashboard;
  state.agents = agents;
  state.environments = environments;
  state.traces = traces;
  state.logs = logs;
  state.alerts = alerts;
  state.alertRules = alertRules;
  state.alertHistory = alertHistory;
  state.hitl = hitl;
  state.billing = billing;
  state.billingUsage = billingUsage;
  state.audit = audit;
  state.evaluations = evaluations.runs || [];
  state.annotationQueues = evaluations.queues || [];
  state.catalogTemplates = catalogTemplates;
  state.teamMembers = teamMembers;
  state.apiExplorer = apiExplorer;
  if (!state.environments.some((item) => item.name === state.selectedEnvironment)) {
    state.selectedEnvironment = state.environments[0]?.name || "production";
  }
  await loadEnvironmentScopedData();
  render();
}

async function safeLoadEnvironmentScopedData() {
  try {
    await loadEnvironmentScopedData();
    render();
  } catch (error) {
    handleApiError(error);
  }
}

async function loadEnvironmentScopedData() {
  await Promise.all([loadSecrets(), maybeSelectAgent()]);
}

async function loadSecrets() {
  if (!state.selectedEnvironment) {
    state.secrets = [];
    return;
  }
  const response = await api(
    `/secrets?environment=${encodeURIComponent(state.selectedEnvironment)}`
  );
  state.secrets = (response.keys || []).map((key) => ({ key }));
}

async function maybeSelectAgent() {
  const envAgents = filteredAgents();
  const nextAgent =
    state.selectedAgent && envAgents.some((item) => item.name === state.selectedAgent)
      ? state.selectedAgent
      : envAgents[0]?.name || null;
  state.selectedAgent = nextAgent;
  if (!nextAgent) {
    state.agentDetail = null;
    state.agentStatus = null;
    state.agentHistory = [];
    return;
  }
  await loadAgent(nextAgent);
}

async function loadAgent(agentName) {
  const environment = state.selectedEnvironment;
  const [detail, status, history] = await Promise.all([
    api(`/agents/${encodeURIComponent(agentName)}?environment=${encodeURIComponent(environment)}`),
    api(
      `/deployments/status?environment=${encodeURIComponent(environment)}&agent=${encodeURIComponent(
        agentName
      )}`
    ),
    api(
      `/deployments/history?environment=${encodeURIComponent(environment)}&agent=${encodeURIComponent(
        agentName
      )}`
    ),
  ]);
  state.selectedAgent = agentName;
  state.agentDetail = detail;
  state.agentStatus = status;
  state.agentHistory = history.items || [];
}

async function handleDocumentClick(event) {
  const target = event.target.closest("[data-action]");
  if (!target) {
    return;
  }
  const { action } = target.dataset;
  try {
    if (action === "submit-form") {
      const form = target.closest("form[data-form]");
      if (form) {
        await submitConsoleForm(form);
      }
      return;
    }
    if (action === "select-agent") {
      await loadAgent(target.dataset.agent);
      state.selectedView = "agents";
      render();
      return;
    }
    if (action === "use-environment") {
      state.selectedEnvironment = target.dataset.environment;
      state.selectedAgent = null;
      elements.environmentSelect.value = state.selectedEnvironment;
      await loadEnvironmentScopedData();
      render();
      return;
    }
    if (action === "promote" || action === "rollback") {
      const deploymentId = state.agentStatus?.deployment?.id;
      if (!deploymentId) {
        showNotice("No deployment is selected.", true);
        return;
      }
      await api(`/deployments/${deploymentId}/${action}`, { method: "POST" });
      await safeRefresh(
        action === "promote"
          ? `Promoted ${state.selectedAgent} in ${state.selectedEnvironment}.`
          : `Rolled back ${state.selectedAgent} in ${state.selectedEnvironment}.`
      );
      return;
    }
    if (action === "restart-agent") {
      if (!state.selectedAgent) {
        showNotice("Select an agent first.", true);
        return;
      }
      await api(
        `/agents/${encodeURIComponent(state.selectedAgent)}/restart?environment=${encodeURIComponent(
          state.selectedEnvironment
        )}`,
        { method: "POST" }
      );
      await safeRefresh(`Restarted ${state.selectedAgent} in ${state.selectedEnvironment}.`);
      return;
    }
    if (action === "deploy-template") {
      await api(`/catalog/${encodeURIComponent(target.dataset.template)}/deploy`, {
        method: "POST",
        body: JSON.stringify({
          environment: state.selectedEnvironment,
          workspaceId: state.session.workspaceId,
        }),
      });
      await safeRefresh(
        `Deployed ${target.dataset.template} into ${state.selectedEnvironment}.`
      );
      return;
    }
    if (action === "suppress-rule") {
      await api(`/alerts/rules/${encodeURIComponent(target.dataset.ruleId)}/suppress`, {
        method: "POST",
      });
      await safeRefresh("Alert rule suppressed.");
      return;
    }
    if (action === "approve-hitl" || action === "reject-hitl") {
      const suffix = action === "approve-hitl" ? "approve" : "reject";
      await api(`/hitl/${encodeURIComponent(target.dataset.checkpointId)}/${suffix}`, {
        method: "POST",
        body: JSON.stringify({
          reviewer: state.session?.userId || "operator",
        }),
      });
      await safeRefresh(
        action === "approve-hitl" ? "Checkpoint approved." : "Checkpoint rejected."
      );
      return;
    }
    if (action === "remove-member") {
      await api(`/team/${encodeURIComponent(target.dataset.memberId)}`, {
        method: "DELETE",
      });
      await safeRefresh("Team member removed.");
      return;
    }
    if (action === "revoke-key") {
      await api(`/api-keys/${encodeURIComponent(target.dataset.keyId)}`, {
        method: "DELETE",
      });
      await safeRefresh("API key revoked.");
      return;
    }
    if (action === "delete-secret") {
      await api(
        `/secrets/${encodeURIComponent(target.dataset.secretKey)}?environment=${encodeURIComponent(
          state.selectedEnvironment
        )}`,
        { method: "DELETE" }
      );
      await safeRefresh(`Deleted ${target.dataset.secretKey}.`);
      return;
    }
    if (action === "copy-curl") {
      await copyText(target.dataset.command);
      showNotice("Copied API example.");
      return;
    }
    if (action === "copy-latest-key") {
      await copyText(state.latestApiKeyToken || "");
      showNotice("Copied API key token.");
      return;
    }
    if (action === "download-openapi") {
      const payload = await api("/openapi.json");
      downloadJson("adk-openapi.json", payload);
      showNotice("Downloaded OpenAPI spec.");
      return;
    }
    if (action === "export-audit") {
      const payload = await api("/audit/export", { method: "POST" });
      downloadJson("adk-audit-export.json", payload);
      showNotice("Downloaded audit export.");
    }
  } catch (error) {
    handleApiError(error);
  }
}

async function handleDocumentSubmit(event) {
  const form = event.target.closest("form[data-form]");
  if (!form) {
    return;
  }
  event.preventDefault();
  await submitConsoleForm(form);
}

async function submitConsoleForm(form) {
  const data = new FormData(form);
  const formName = form.dataset.form;
  try {
    if (formName === "run-evaluation") {
      await api("/evaluations", {
        method: "POST",
        body: JSON.stringify({
          agent: data.get("agent") || state.selectedAgent,
          environment: state.selectedEnvironment,
          dataset: data.get("dataset"),
          label: data.get("label") || undefined,
        }),
      });
      form.reset();
      await safeRefresh("Evaluation queued.");
      return;
    }
    if (formName === "invite-team") {
      await api("/team", {
        method: "POST",
        body: JSON.stringify({
          email: data.get("email"),
          role: data.get("role"),
          name: data.get("name") || undefined,
        }),
      });
      form.reset();
      await safeRefresh("Team invitation sent.");
      return;
    }
    if (formName === "create-alert-rule") {
      await api("/alerts/rules", {
        method: "POST",
        body: JSON.stringify({
          name: data.get("name"),
          condition: data.get("condition"),
          channel: data.get("channel"),
        }),
      });
      form.reset();
      await safeRefresh("Alert rule created.");
      return;
    }
    if (formName === "create-api-key") {
      const scopes = String(data.get("scopes") || "")
        .split(",")
        .map((item) => item.trim())
        .filter(Boolean);
      const response = await api("/api-keys", {
        method: "POST",
        body: JSON.stringify({
          name: data.get("name"),
          scopes,
        }),
      });
      state.latestApiKeyToken = response.token;
      form.reset();
      await safeRefresh("API key created.");
      return;
    }
    if (formName === "create-environment") {
      const name = String(data.get("name") || "");
      await api("/environments", {
        method: "POST",
        body: JSON.stringify({
          name,
          region: data.get("region"),
        }),
      });
      state.selectedEnvironment = name;
      form.reset();
      await safeRefresh(`Created environment ${name}.`);
      return;
    }
    if (formName === "promote-environment") {
      await api(`/environments/${encodeURIComponent(state.selectedEnvironment)}/promote`, {
        method: "POST",
        body: JSON.stringify({
          sourceEnvironment: data.get("sourceEnvironment"),
          agentName: data.get("agentName"),
          workspaceId: state.session.workspaceId,
        }),
      });
      await safeRefresh(`Promoted deployment into ${state.selectedEnvironment}.`);
      return;
    }
    if (formName === "change-tier") {
      await api("/billing/tier", {
        method: "POST",
        body: JSON.stringify({
          tier: data.get("tier"),
        }),
      });
      await safeRefresh(`Workspace plan changed to ${data.get("tier")}.`);
      return;
    }
    if (formName === "set-secret") {
      const key = String(data.get("key") || "");
      await api("/secrets", {
        method: "POST",
        body: JSON.stringify({
          environment: state.selectedEnvironment,
          key,
          value: data.get("value"),
        }),
      });
      form.reset();
      await safeRefresh(`Stored ${key} in ${state.selectedEnvironment}.`);
      return;
    }
    if (formName === "scale-agent") {
      if (!state.selectedAgent) {
        showNotice("Select an agent first.", true);
        return;
      }
      await api(
        `/agents/${encodeURIComponent(state.selectedAgent)}/scale?environment=${encodeURIComponent(
          state.selectedEnvironment
        )}`,
        {
          method: "POST",
          body: JSON.stringify({
            minInstances: Number(data.get("minInstances")),
            maxInstances: Number(data.get("maxInstances")),
          }),
        }
      );
      await safeRefresh(`Updated scaling for ${state.selectedAgent}.`);
      return;
    }
    if (formName === "settings") {
      const autoRefreshSeconds = Math.max(0, Number(data.get("autoRefreshSeconds")) || 0);
      state.settings.autoRefreshSeconds = autoRefreshSeconds;
      persistSettings(state.settings);
      scheduleAutoRefresh();
      render();
      showNotice(
        autoRefreshSeconds
          ? `Auto-refresh set to ${autoRefreshSeconds}s.`
          : "Auto-refresh disabled."
      );
    }
  } catch (error) {
    handleApiError(error);
  }
}

function render() {
  const loggedIn = Boolean(state.session?.token);
  elements.loginScreen.classList.toggle("hidden", loggedIn);
  elements.appShell.classList.toggle("hidden", !loggedIn);
  if (!loggedIn) {
    elements.content.innerHTML = "";
    return;
  }
  elements.workspaceLabel.textContent = state.dashboard?.workspace?.name || "Workspace";
  elements.viewTitle.textContent = titleForView(state.selectedView);
  renderEnvironmentSelect();
  for (const item of elements.navItems) {
    item.classList.toggle("active", item.dataset.view === state.selectedView);
  }
  elements.content.innerHTML = state.dashboard ? renderView() : renderLoading();
  bindDynamicHandlers();
}

function renderEnvironmentSelect() {
  if (!state.environments.length) {
    elements.environmentSelect.innerHTML = '<option value="production">production</option>';
    return;
  }
  elements.environmentSelect.innerHTML = state.environments
    .map(
      (item) => `
        <option value="${escapeHtml(item.name)}" ${
          item.name === state.selectedEnvironment ? "selected" : ""
        }>
          ${escapeHtml(item.name)}
        </option>
      `
    )
    .join("");
}

function renderView() {
  switch (state.selectedView) {
    case "dashboard":
      return renderDashboard();
    case "agents":
      return renderAgents();
    case "traces":
      return renderTableCard({
        title: "Recent Traces",
        subtitle: "Inference traces across active agents.",
        columns: [
          { key: "id", label: "Trace" },
          { key: "status", label: "Status" },
          { key: "model", label: "Model" },
          { key: "duration", label: "Duration" },
          { key: "tokens", label: "Tokens" },
          { key: "step", label: "Step" },
        ],
        rows: state.traces,
        emptyMessage: "No traces have been captured yet.",
      });
    case "logs":
      return renderTableCard({
        title: "Runtime Logs",
        subtitle: "Recent log events from deployed instances.",
        columns: [
          { key: "time", label: "Time" },
          { key: "level", label: "Level" },
          { key: "instance", label: "Instance" },
          { key: "message", label: "Message" },
        ],
        rows: state.logs,
        emptyMessage: "No logs are available yet.",
      });
    case "evaluations":
      return renderEvaluations();
    case "catalog":
      return renderCatalog();
    case "environments":
      return renderEnvironments();
    case "alerts":
      return renderAlerts();
    case "hitl":
      return renderHitl();
    case "team":
      return renderTeam();
    case "billing":
      return renderBilling();
    case "api":
      return renderApi();
    case "audit":
      return renderAudit();
    case "settings":
      return renderSettings();
    default:
      return renderDashboard();
  }
}

function renderDashboard() {
  const environment = state.environments.find((item) => item.name === state.selectedEnvironment);
  const selectedAgents = filteredAgents();
  const activeStrategy = state.dashboard?.activeStrategy || "rolling";
  return `
    <section class="hero">
      <article class="panel">
        <p class="eyebrow">Workspace Summary</p>
        <h2>${escapeHtml(state.dashboard.workspace.name)}</h2>
        <p class="lede">
          ${escapeHtml(state.dashboard.workspace.plan)} plan in ${escapeHtml(
            state.dashboard.workspace.region
          )}. ${selectedAgents.length} agents are visible in ${escapeHtml(
            state.selectedEnvironment
          )}.
        </p>
        <div class="chip-row">
          <span class="chip success">Strategy ${escapeHtml(activeStrategy)}</span>
          <span class="chip ${statusTone(environment?.status || "")}">${escapeHtml(
            environment?.status || "Unknown"
          )}</span>
          <span class="chip">Workspace ${escapeHtml(state.dashboard.workspace.id)}</span>
        </div>
        <div class="rollout-strip">
          ${[
            ["Build", "Bundle created and validated"],
            ["Validate", "Manifest, secrets, and rollout checks passed"],
            [
              "Deploy",
              `Latest phase: ${state.agentStatus?.deployment?.rolloutPhase || "idle"}`,
            ],
            ["Observe", "Metrics, logs, traces, alerts, and audit online"],
          ]
            .map(
              ([label, copy]) => `
                <div class="rollout-step">
                  <strong>${escapeHtml(label)}</strong>
                  <p class="helper">${escapeHtml(copy)}</p>
                </div>
              `
            )
            .join("")}
        </div>
      </article>
      <article class="panel">
        <p class="eyebrow">Operator Status</p>
        <h2>Live platform surface</h2>
        <p class="lede">
          Evaluations, catalog deploys, team access, environment promotion, alerting, HITL, API
          keys, and audit export are all backed by the control plane.
        </p>
        <div class="bar-row">
          ${selectedAgents.slice(0, 4).map(renderHealthBar).join("")}
        </div>
      </article>
    </section>
    <section class="grid-4">
      ${renderMetricCard("Agents", String(state.agents.length), "Live deployable agents")}
      ${renderMetricCard("Evaluations", String(state.evaluations.length), "Stored evaluation runs")}
      ${renderMetricCard("Alert Rules", String(state.alertRules.length), "Configured alert policies")}
      ${renderMetricCard("HITL Queue", String(state.hitl.length), "Open human checkpoints")}
    </section>
    <section class="grid-2">
      ${renderTableCard({
        title: "Agents In Scope",
        columns: [
          { key: "name", label: "Agent" },
          { key: "health", label: "Health" },
          { key: "version", label: "Version" },
          { key: "sourceKind", label: "Source" },
        ],
        rows: selectedAgents,
        emptyMessage: "No agents are deployed to the selected environment yet.",
      })}
      ${renderTableCard({
        title: "Environments",
        columns: [
          { key: "name", label: "Environment" },
          { key: "status", label: "Status" },
          { key: "agents", label: "Agents" },
          { key: "region", label: "Region" },
        ],
        rows: state.environments,
        emptyMessage: "No environments are available.",
      })}
    </section>
    <section class="grid-3">
      ${renderTableCard({
        title: "Alerts",
        columns: [
          { key: "name", label: "Alert" },
          { key: "state", label: "State" },
          { key: "description", label: "Description" },
        ],
        rows: state.alerts,
        emptyMessage: "No active alerts.",
      })}
      ${renderTableCard({
        title: "Recent Logs",
        columns: [
          { key: "time", label: "Time" },
          { key: "level", label: "Level" },
          { key: "message", label: "Message" },
        ],
        rows: state.logs,
        emptyMessage: "No logs captured yet.",
      })}
      ${renderTableCard({
        title: "Recent Traces",
        columns: [
          { key: "id", label: "Trace" },
          { key: "status", label: "Status" },
          { key: "model", label: "Model" },
        ],
        rows: state.traces,
        emptyMessage: "No traces captured yet.",
      })}
    </section>
  `;
}

function renderAgents() {
  const agents = filteredAgents();
  if (!agents.length) {
    return renderEmptyState(
      "Agents",
      "No agents are deployed to this environment yet.",
      "Deploy a catalog template or push a new bundle to see it here."
    );
  }
  return `
    <section class="split">
      <article class="table-card">
        <p class="eyebrow">Agents</p>
        <h2>Deployments in ${escapeHtml(state.selectedEnvironment)}</h2>
        <div class="agent-list">
          ${agents
            .map(
              (agent) => `
                <button
                  class="agent-row ${agent.name === state.selectedAgent ? "active" : ""}"
                  data-action="select-agent"
                  data-agent="${escapeHtml(agent.name)}"
                >
                  <strong>${escapeHtml(agent.name)}</strong>
                  <p class="helper">
                    ${escapeHtml(agent.version)} · ${escapeHtml(agent.requestRate)} · ${escapeHtml(
                      agent.latencyP95
                    )}
                  </p>
                  <div class="pill-row">
                    <span class="pill ${statusTone(agent.health)}">${escapeHtml(
                      agent.health
                    )}</span>
                    <span class="pill">${escapeHtml(agent.sourceKind)}</span>
                  </div>
                </button>
              `
            )
            .join("")}
        </div>
      </article>
      <article class="detail-card">
        ${state.agentDetail ? renderAgentDetail() : renderEmptyState("Agent", "Choose an agent to inspect.")}
      </article>
    </section>
  `;
}

function renderAgentDetail() {
  const detail = state.agentDetail;
  const status = state.agentStatus?.deployment;
  const metrics = state.agentStatus?.metrics;
  const minInstances = status?.manifest?.scaling?.minInstances || 1;
  const maxInstances = status?.manifest?.scaling?.maxInstances || 2;
  return `
    <div class="detail-stack">
      <div class="detail-head">
        <div>
          <p class="eyebrow">Agent Detail</p>
          <h2>${escapeHtml(detail.name)}</h2>
          <p class="lede">${escapeHtml(detail.description)}</p>
          <div class="pill-row">
            <span class="pill success">${escapeHtml(detail.strategy)}</span>
            <span class="pill">${escapeHtml(detail.deploymentSource)}</span>
            <span class="pill">${escapeHtml(detail.scalingPolicy)}</span>
          </div>
        </div>
        <div class="detail-actions">
          <button class="action-button" data-action="promote">Promote</button>
          <button class="ghost-button" data-action="restart-agent">Restart</button>
          <button class="ghost-button danger" data-action="rollback">Rollback</button>
        </div>
      </div>
      <div class="mini-grid">
        <div class="mini-card">
          <p class="metric-title">Endpoint</p>
          <div class="mono">${escapeHtml(detail.endpoint)}</div>
        </div>
        <div class="mini-card">
          <p class="metric-title">Rollout Phase</p>
          <div>${escapeHtml(status?.rolloutPhase || "unknown")}</div>
        </div>
        <div class="mini-card">
          <p class="metric-title">Live Metrics</p>
          <div>${escapeHtml(metrics?.latencyP95 || detail.errorRate)}</div>
          <p class="helper">${escapeHtml(metrics?.requestRate || "No metric stream")}</p>
        </div>
      </div>
      <section class="grid-2">
        ${renderTableCard({
          title: "Active Instances",
          columns: [
            { key: "id", label: "Instance" },
            { key: "state", label: "State" },
            { key: "stats", label: "Stats" },
          ],
          rows: detail.instances,
          emptyMessage: "No instances are registered.",
        })}
        ${renderTableCard({
          title: "Deployment History",
          columns: [
            { key: "version", label: "Version" },
            { key: "createdAt", label: "Timestamp" },
            { key: "status", label: "Status" },
            { key: "sourceKind", label: "Source" },
          ],
          rows: state.agentHistory,
          emptyMessage: "No deployment history yet.",
        })}
      </section>
      <section class="grid-2">
        <article class="panel">
          <p class="eyebrow">Scale Override</p>
          <h2>Update min and max instances</h2>
          <form class="form-grid" data-form="scale-agent">
            <label>
              <span>Min Instances</span>
              <input name="minInstances" type="number" min="1" value="${minInstances}" required />
            </label>
            <label>
              <span>Max Instances</span>
              <input name="maxInstances" type="number" min="1" value="${maxInstances}" required />
            </label>
            <button type="button" class="action-button" data-action="submit-form">
              Apply override
            </button>
          </form>
        </article>
        <article class="panel">
          <p class="eyebrow">Latency And Load</p>
          <div class="bar-row">
            ${(detail.metrics || []).map(renderMetricBar).join("")}
          </div>
        </article>
      </section>
    </div>
  `;
}

function renderEvaluations() {
  const runs = state.evaluations.filter(
    (item) => item.environment === state.selectedEnvironment
  );
  return `
    <section class="grid-4">
      ${renderMetricCard("Runs", String(runs.length), "In selected environment")}
      ${renderMetricCard("Queues", String(state.annotationQueues.length), "Annotation queues")}
      ${renderMetricCard(
        "Passing",
        String(runs.filter((item) => item.status === "passed").length),
        "Passing regression checks"
      )}
      ${renderMetricCard("Selected Agent", state.selectedAgent || "None", "Default target")}
    </section>
    <section class="grid-2">
      <article class="panel">
        <p class="eyebrow">Run Evaluation</p>
        <h2>Trigger a new suite</h2>
        <form class="form-grid" data-form="run-evaluation">
          <label>
            <span>Agent</span>
            <input name="agent" value="${escapeHtml(state.selectedAgent || "")}" required />
          </label>
          <label>
            <span>Dataset</span>
            <input name="dataset" value="${escapeHtml(
              `${state.selectedEnvironment}-golden`
            )}" required />
          </label>
          <label class="full-span">
            <span>Label</span>
            <input name="label" placeholder="Optional run label" />
          </label>
          <button type="button" class="action-button" data-action="submit-form">
            Run evaluation
          </button>
        </form>
      </article>
      ${renderTableCard({
        title: "Annotation Queues",
        subtitle: "Queues backing reviewer workflows.",
        columns: [
          { key: "name", label: "Queue" },
          { key: "pendingItems", label: "Pending" },
          {
            label: "Reviewers",
            render: (row) => escapeHtml((row.reviewers || []).join(", ")),
          },
          { key: "rule", label: "Rule" },
        ],
        rows: state.annotationQueues,
        emptyMessage: "No annotation queues configured.",
      })}
    </section>
    ${renderTableCard({
      title: "Evaluation Runs",
      subtitle: "Stored runs visible to the console.",
      columns: [
        { key: "label", label: "Run" },
        { key: "agent", label: "Agent" },
        { key: "dataset", label: "Dataset" },
        { key: "status", label: "Status" },
        { key: "score", label: "Score" },
        { key: "createdAt", label: "Created" },
      ],
      rows: runs,
      emptyMessage: "No evaluation runs for this environment yet.",
    })}
  `;
}

function renderCatalog() {
  return `
    <section class="grid-3">
      ${state.catalogTemplates
        .map(
          (template) => `
            <article class="panel">
              <p class="eyebrow">${escapeHtml(template.source)}</p>
              <h2>${escapeHtml(template.name)}</h2>
              <p class="lede">${escapeHtml(template.summary)}</p>
              <div class="pill-row">
                <span class="pill success">${escapeHtml(template.strategy)}</span>
                <span class="pill">Recommended ${escapeHtml(
                  template.recommendedEnvironment
                )}</span>
              </div>
              <div class="action-row">
                <button
                  class="action-button"
                  data-action="deploy-template"
                  data-template="${escapeHtml(template.id)}"
                >
                  Deploy to ${escapeHtml(state.selectedEnvironment)}
                </button>
              </div>
            </article>
          `
        )
        .join("")}
    </section>
  `;
}

function renderEnvironments() {
  const sourceOptions = state.environments.filter(
    (item) => item.name !== state.selectedEnvironment
  );
  return `
    <section class="grid-3">
      ${state.environments
        .map(
          (environment) => `
            <article class="panel ${environment.name === state.selectedEnvironment ? "selected-panel" : ""}">
              <p class="eyebrow">${escapeHtml(environment.name)}</p>
              <h2>${escapeHtml(environment.status)}</h2>
              <p class="lede">
                ${escapeHtml(environment.region)} region with ${escapeHtml(
                  String(environment.agents)
                )} deployed agents.
              </p>
              <div class="pill-row">
                <span class="pill ${statusTone(environment.status)}">${escapeHtml(
                  environment.status
                )}</span>
                <span class="pill">${escapeHtml(String(environment.agents))} agents</span>
              </div>
              <div class="action-row">
                <button
                  class="ghost-button"
                  data-action="use-environment"
                  data-environment="${escapeHtml(environment.name)}"
                >
                  Inspect
                </button>
              </div>
            </article>
          `
        )
        .join("")}
    </section>
    <section class="grid-2">
      <article class="panel">
        <p class="eyebrow">Create Environment</p>
        <h2>Add a new runtime target</h2>
        <form class="form-grid" data-form="create-environment">
          <label>
            <span>Name</span>
            <input name="name" placeholder="sandbox" required />
          </label>
          <label>
            <span>Region</span>
            <input name="region" value="US" required />
          </label>
          <button type="button" class="action-button" data-action="submit-form">
            Create environment
          </button>
        </form>
      </article>
      <article class="panel">
        <p class="eyebrow">Promote Deployment</p>
        <h2>Copy a release into ${escapeHtml(state.selectedEnvironment)}</h2>
        <form class="form-grid" data-form="promote-environment">
          <label>
            <span>Source Environment</span>
            <select name="sourceEnvironment" ${sourceOptions.length ? "" : "disabled"}>
              ${sourceOptions
                .map(
                  (item) => `
                    <option value="${escapeHtml(item.name)}">${escapeHtml(item.name)}</option>
                  `
                )
                .join("")}
            </select>
          </label>
          <label>
            <span>Agent Name</span>
            <input
              name="agentName"
              value="${escapeHtml(state.selectedAgent || filteredAgents()[0]?.name || "")}"
              required
            />
          </label>
          <button type="button" class="action-button" data-action="submit-form" ${
            sourceOptions.length ? "" : "disabled"
          }>
            Promote deployment
          </button>
        </form>
      </article>
    </section>
    <section class="grid-2">
      ${renderTableCard({
        title: `Secrets in ${state.selectedEnvironment}`,
        subtitle: "Environment-scoped secrets stored in the control plane.",
        columns: [
          { key: "key", label: "Key" },
          {
            label: "Actions",
            render: (row) => `
              <div class="table-actions">
                <button
                  class="ghost-button danger small-button"
                  data-action="delete-secret"
                  data-secret-key="${escapeAttribute(row.key)}"
                >
                  Delete
                </button>
              </div>
            `,
          },
        ],
        rows: state.secrets,
        emptyMessage: "No secrets stored in this environment.",
      })}
      <article class="panel">
        <p class="eyebrow">Set Secret</p>
        <h2>Rotate environment credentials</h2>
        <form class="form-grid" data-form="set-secret">
          <label>
            <span>Key</span>
            <input name="key" placeholder="OPENAI_API_KEY" required />
          </label>
          <label>
            <span>Value</span>
            <input
              name="value"
              type="password"
              placeholder="secret"
              autocomplete="new-password"
              required
            />
          </label>
          <button type="button" class="action-button" data-action="submit-form">
            Store secret
          </button>
        </form>
      </article>
    </section>
  `;
}

function renderAlerts() {
  return `
    <section class="grid-4">
      ${renderMetricCard("Summaries", String(state.alerts.length), "Surface alerts")}
      ${renderMetricCard("Rules", String(state.alertRules.length), "Configured policies")}
      ${renderMetricCard("History", String(state.alertHistory.length), "Recorded events")}
      ${renderMetricCard(
        "Suppressed",
        String(state.alertRules.filter((item) => item.status === "suppressed").length),
        "Rules in maintenance"
      )}
    </section>
    <section class="grid-2">
      <article class="panel">
        <p class="eyebrow">Create Rule</p>
        <h2>Add a health or policy trigger</h2>
        <form class="form-grid" data-form="create-alert-rule">
          <label>
            <span>Name</span>
            <input name="name" placeholder="Latency p95 > 900ms" required />
          </label>
          <label>
            <span>Channel</span>
            <input name="channel" value="pagerduty" required />
          </label>
          <label class="full-span">
            <span>Condition</span>
            <input name="condition" placeholder="p95 latency above 900ms for 10m" required />
          </label>
          <button type="button" class="action-button" data-action="submit-form">
            Create rule
          </button>
        </form>
      </article>
      ${renderTableCard({
        title: "Alert Summaries",
        subtitle: "Current alert surface exposed on the dashboard.",
        columns: [
          { key: "name", label: "Alert" },
          { key: "state", label: "State" },
          { key: "description", label: "Description" },
        ],
        rows: state.alerts,
        emptyMessage: "No alert summaries are active.",
      })}
    </section>
    <section class="grid-2">
      ${renderTableCard({
        title: "Alert Rules",
        columns: [
          { key: "name", label: "Rule" },
          { key: "condition", label: "Condition" },
          { key: "channel", label: "Channel" },
          { key: "status", label: "Status" },
          {
            label: "Actions",
            render: (row) => `
              <div class="table-actions">
                <button
                  class="ghost-button small-button"
                  data-action="suppress-rule"
                  data-rule-id="${escapeAttribute(row.id)}"
                >
                  Suppress
                </button>
              </div>
            `,
          },
        ],
        rows: state.alertRules,
        emptyMessage: "No alert rules configured.",
      })}
      ${renderTableCard({
        title: "Alert History",
        columns: [
          { key: "ruleName", label: "Rule" },
          { key: "state", label: "State" },
          { key: "triggeredAt", label: "Triggered" },
          { key: "detail", label: "Detail" },
        ],
        rows: state.alertHistory,
        emptyMessage: "No alert history recorded yet.",
      })}
    </section>
  `;
}

function renderHitl() {
  return `
    <section class="grid-4">
      ${renderMetricCard("Pending", String(state.hitl.filter((item) => item.state === "Pending").length), "Waiting for review")}
      ${renderMetricCard("Approved", String(state.hitl.filter((item) => item.state === "Approved").length), "Approved checkpoints")}
      ${renderMetricCard("Rejected", String(state.hitl.filter((item) => item.state === "Rejected").length), "Rejected checkpoints")}
      ${renderMetricCard("Reviewer", state.session?.userId || "operator", "Default reviewer")}
    </section>
    ${renderTableCard({
      title: "Human In The Loop Queue",
      subtitle: "Approve or reject checkpoints directly from the console.",
      columns: [
        { key: "id", label: "Checkpoint" },
        { key: "agent", label: "Agent" },
        { key: "checkpointType", label: "Type" },
        { key: "wait", label: "Wait" },
        { key: "state", label: "State" },
        {
          label: "Actions",
          render: (row) => `
            <div class="table-actions">
              <button
                class="action-button small-button"
                data-action="approve-hitl"
                data-checkpoint-id="${escapeAttribute(row.id)}"
              >
                Approve
              </button>
              <button
                class="ghost-button danger small-button"
                data-action="reject-hitl"
                data-checkpoint-id="${escapeAttribute(row.id)}"
              >
                Reject
              </button>
            </div>
          `,
        },
      ],
      rows: state.hitl,
      emptyMessage: "No checkpoints are waiting for review.",
    })}
  `;
}

function renderTeam() {
  return `
    <section class="grid-2">
      <article class="panel">
        <p class="eyebrow">Invite Team Member</p>
        <h2>Add operators and reviewers</h2>
        <form class="form-grid" data-form="invite-team">
          <label>
            <span>Name</span>
            <input name="name" placeholder="Optional display name" />
          </label>
          <label>
            <span>Email</span>
            <input name="email" type="email" placeholder="new.operator@example.com" required />
          </label>
          <label>
            <span>Role</span>
            <input name="role" value="runtime_engineer" required />
          </label>
          <button type="button" class="action-button" data-action="submit-form">
            Send invite
          </button>
        </form>
      </article>
      ${renderTableCard({
        title: "Workspace Team",
        subtitle: "Current operators and reviewers with live access.",
        columns: [
          { key: "name", label: "Member" },
          { key: "email", label: "Email" },
          { key: "role", label: "Role" },
          { key: "lastActive", label: "Last Active" },
          { key: "invitationStatus", label: "Status" },
          {
            label: "Actions",
            render: (row) => `
              <div class="table-actions">
                <button
                  class="ghost-button danger small-button"
                  data-action="remove-member"
                  data-member-id="${escapeAttribute(row.id)}"
                >
                  Remove
                </button>
              </div>
            `,
          },
        ],
        rows: state.teamMembers,
        emptyMessage: "No team members exist yet.",
      })}
    </section>
  `;
}

function renderBilling() {
  return `
    <section class="grid-4">
      ${state.billing.map((item) => renderMetricCard(item.label, item.value, item.sub)).join("")}
    </section>
    <section class="grid-2">
      ${renderTableCard({
        title: "Usage Breakdown",
        subtitle: "Live limits and current workspace usage.",
        columns: [
          { key: "label", label: "Line Item" },
          { key: "current", label: "Current" },
          { key: "limit", label: "Limit" },
          { key: "unit", label: "Unit" },
        ],
        rows: state.billingUsage,
        emptyMessage: "No billing usage is available yet.",
      })}
      <article class="panel">
        <p class="eyebrow">Change Tier</p>
        <h2>Workspace plan</h2>
        <p class="lede">Current plan: ${escapeHtml(state.dashboard.workspace.plan)}</p>
        <form class="form-grid" data-form="change-tier">
          <label>
            <span>Tier</span>
            <select name="tier">
              ${["Free", "Pro", "Enterprise"]
                .map(
                  (tier) => `
                    <option value="${escapeHtml(tier)}" ${
                      tier === state.dashboard.workspace.plan ? "selected" : ""
                    }>
                      ${escapeHtml(tier)}
                    </option>
                  `
                )
                .join("")}
            </select>
          </label>
          <button type="button" class="action-button" data-action="submit-form">
            Update plan
          </button>
        </form>
      </article>
    </section>
  `;
}

function renderApi() {
  return `
    <section class="grid-2">
      <article class="panel">
        <p class="eyebrow">API Surface</p>
        <h2>OpenAPI and endpoint explorer</h2>
        <p class="mono code-block">${escapeHtml(state.apiExplorer.openapiUrl)}</p>
        <div class="action-row">
          <button class="action-button" data-action="download-openapi">Download OpenAPI</button>
        </div>
      </article>
      <article class="panel">
        <p class="eyebrow">Create API Key</p>
        <h2>Issue machine credentials</h2>
        <form class="form-grid" data-form="create-api-key">
          <label>
            <span>Name</span>
            <input name="name" placeholder="ci-bot" required />
          </label>
          <label class="full-span">
            <span>Scopes</span>
            <input
              name="scopes"
              value="deploy:read, deploy:write"
              placeholder="deploy:read, deploy:write"
            />
          </label>
          <button type="button" class="action-button" data-action="submit-form">
            Create key
          </button>
        </form>
      </article>
    </section>
    ${
      state.latestApiKeyToken
        ? `
          <section class="grid-1">
            <article class="panel">
              <p class="eyebrow">New API Key</p>
              <h2>Copy the token now</h2>
              <p class="mono code-block">${escapeHtml(state.latestApiKeyToken)}</p>
              <div class="action-row">
                <button class="ghost-button" data-action="copy-latest-key">Copy token</button>
              </div>
            </article>
          </section>
        `
        : ""
    }
    <section class="grid-2">
      ${renderTableCard({
        title: "API Keys",
        subtitle: "Stored machine credentials accepted by bearer auth.",
        columns: [
          { key: "name", label: "Name" },
          { key: "preview", label: "Preview" },
          {
            label: "Scopes",
            render: (row) => escapeHtml((row.scopes || []).join(", ")),
          },
          { key: "lastUsed", label: "Last Used" },
          {
            label: "Actions",
            render: (row) => `
              <div class="table-actions">
                <button
                  class="ghost-button danger small-button"
                  data-action="revoke-key"
                  data-key-id="${escapeAttribute(row.id)}"
                >
                  Revoke
                </button>
              </div>
            `,
          },
        ],
        rows: state.apiExplorer.apiKeys || [],
        emptyMessage: "No API keys have been issued yet.",
      })}
      ${renderTableCard({
        title: "Endpoint Explorer",
        subtitle: "Primary control-plane routes exposed to operators and automation.",
        columns: [
          { key: "method", label: "Method" },
          { key: "path", label: "Path" },
          { key: "description", label: "Description" },
          { key: "auth", label: "Auth" },
          {
            label: "Example",
            render: (row) => `
              <div class="table-actions wrap-actions">
                <button
                  class="ghost-button small-button"
                  data-action="copy-curl"
                  data-command="${escapeAttribute(row.sampleCurl)}"
                >
                  Copy curl
                </button>
              </div>
            `,
          },
        ],
        rows: state.apiExplorer.endpoints || [],
        emptyMessage: "No endpoint docs are available.",
      })}
    </section>
  `;
}

function renderAudit() {
  return `
    <section class="grid-2">
      <article class="panel">
        <p class="eyebrow">Export Audit</p>
        <h2>Immutable operator activity log</h2>
        <p class="lede">
          Export the full audit stream as JSON for compliance review or downstream storage.
        </p>
        <div class="action-row">
          <button class="action-button" data-action="export-audit">Download export</button>
        </div>
      </article>
      ${renderTableCard({
        title: "Audit Events",
        subtitle: "Recent operator and system actions stored by the control plane.",
        columns: [
          { key: "timestamp", label: "Timestamp" },
          { key: "action", label: "Action" },
          { key: "resource", label: "Resource" },
          { key: "result", label: "Result" },
        ],
        rows: state.audit,
        emptyMessage: "No audit events have been recorded yet.",
      })}
    </section>
  `;
}

function renderSettings() {
  return `
    <section class="grid-2">
      <article class="panel">
        <p class="eyebrow">Connection</p>
        <h2>Console session</h2>
        <p class="mono code-block">${escapeHtml(state.config.apiBaseUrl)}</p>
        <p class="helper">
          Workspace ${escapeHtml(state.session.workspaceName || state.session.workspaceId)}
        </p>
        <p class="helper">User ${escapeHtml(state.session.userId || "operator")}</p>
      </article>
      <article class="panel">
        <p class="eyebrow">Auto Refresh</p>
        <h2>Refresh interval</h2>
        <form class="form-grid" data-form="settings">
          <label>
            <span>Seconds</span>
            <input
              name="autoRefreshSeconds"
              type="number"
              min="0"
              value="${escapeHtml(String(state.settings.autoRefreshSeconds))}"
            />
          </label>
          <button type="button" class="action-button" data-action="submit-form">
            Save settings
          </button>
        </form>
      </article>
    </section>
  `;
}

function renderTableCard({ title, subtitle = "", columns, rows, emptyMessage = "No data yet." }) {
  if (!rows.length) {
    return renderEmptyState(title, emptyMessage, subtitle);
  }
  return `
    <article class="table-card">
      <p class="eyebrow">${escapeHtml(title)}</p>
      ${subtitle ? `<p class="table-subtitle">${escapeHtml(subtitle)}</p>` : ""}
      <table>
        <thead>
          <tr>${columns.map((column) => `<th>${escapeHtml(column.label)}</th>`).join("")}</tr>
        </thead>
        <tbody>
          ${rows
            .map(
              (row) => `
                <tr>
                  ${columns
                    .map((column) => `<td>${renderCell(column, row)}</td>`)
                    .join("")}
                </tr>
              `
            )
            .join("")}
        </tbody>
      </table>
    </article>
  `;
}

function renderMetricCard(title, value, subtitle) {
  return `
    <article class="metric-card">
      <p class="metric-title">${escapeHtml(title)}</p>
      <div class="metric-value">${escapeHtml(value)}</div>
      <p class="metric-subtitle">${escapeHtml(subtitle)}</p>
    </article>
  `;
}

function renderMetricBar(item) {
  return `
    <div>
      <div class="table-subtitle">${escapeHtml(item.label)} · ${escapeHtml(String(item.value))}</div>
      <div class="bar"><span style="width:${Math.min(Number(item.value) || 0, 100)}%"></span></div>
    </div>
  `;
}

function renderHealthBar(agent) {
  const width = Math.min(98, 40 + (simpleSeed(agent.name) % 50));
  return `
    <div>
      <div class="table-subtitle">${escapeHtml(agent.name)} · ${escapeHtml(agent.health)}</div>
      <div class="bar"><span style="width:${width}%"></span></div>
    </div>
  `;
}

function renderLoading() {
  return renderEmptyState("Loading", "Fetching control-plane state...", "The console will render once the API responds.");
}

function bindDynamicHandlers() {
  for (const element of elements.content.querySelectorAll("[data-action]")) {
    element.addEventListener("click", handleDocumentClick);
  }
  for (const form of elements.content.querySelectorAll("form[data-form]")) {
    form.addEventListener("submit", handleDocumentSubmit);
  }
}

function renderEmptyState(title, message, copy = "") {
  return `
    <article class="empty-state">
      <div>
        <p class="eyebrow">${escapeHtml(title)}</p>
        <h2>${escapeHtml(message)}</h2>
        ${copy ? `<p class="helper">${escapeHtml(copy)}</p>` : ""}
      </div>
    </article>
  `;
}

function renderCell(column, row) {
  if (column.render) {
    return column.render(row);
  }
  return escapeHtml(readValue(row, column.key));
}

function titleForView(view) {
  const titles = {
    dashboard: "Dashboard",
    agents: "Agents",
    traces: "Traces",
    logs: "Logs",
    evaluations: "Evaluations",
    catalog: "Catalog",
    environments: "Environments",
    alerts: "Alerts",
    hitl: "HITL",
    team: "Team",
    billing: "Billing",
    api: "API",
    audit: "Audit",
    settings: "Settings",
  };
  return titles[view] || "Dashboard";
}

function filteredAgents() {
  return state.agents.filter((item) => item.environment === state.selectedEnvironment);
}

function statusTone(value) {
  const normal = String(value).toLowerCase();
  if (
    normal.includes("healthy") ||
    normal.includes("success") ||
    normal.includes("active") ||
    normal.includes("approved") ||
    normal.includes("passed") ||
    normal.includes("resolved")
  ) {
    return "success";
  }
  if (
    normal.includes("pending") ||
    normal.includes("warn") ||
    normal.includes("degraded") ||
    normal.includes("canary") ||
    normal.includes("configured") ||
    normal.includes("suppressed")
  ) {
    return "warn";
  }
  return "danger";
}

function simpleSeed(text) {
  return Array.from(String(text || "")).reduce(
    (total, character) => total + character.charCodeAt(0),
    0
  );
}

function readValue(row, key) {
  if (!key) {
    return "";
  }
  const snakeCaseKey = key.replace(/[A-Z]/g, (match) => `_${match.toLowerCase()}`);
  return row?.[key] ?? row?.[snakeCaseKey] ?? "";
}

function handleApiError(error, loginMessage = "") {
  if (error?.status === 401) {
    clearSession();
    render();
    showLogin(loginMessage || "Authentication expired.");
    return;
  }
  showNotice(error?.message || "Request failed.", true);
}

function showLogin(message) {
  elements.loginError.textContent = message;
  elements.loginError.classList.remove("hidden");
  elements.loginScreen.classList.remove("hidden");
  elements.appShell.classList.add("hidden");
}

function hideError() {
  elements.loginError.classList.add("hidden");
  elements.loginError.textContent = "";
}

function showNotice(message, isError = false) {
  elements.notice.textContent = message;
  elements.notice.classList.remove("hidden");
  elements.notice.classList.toggle("notice-error", isError);
  window.clearTimeout(showNotice.timer);
  showNotice.timer = window.setTimeout(() => {
    elements.notice.classList.add("hidden");
    elements.notice.classList.remove("notice-error");
  }, 3200);
}

function scheduleAutoRefresh() {
  window.clearInterval(state.autoRefreshTimer);
  state.autoRefreshTimer = null;
  if (!state.session?.token || !state.settings.autoRefreshSeconds) {
    return;
  }
  state.autoRefreshTimer = window.setInterval(() => {
    void safeRefresh();
  }, state.settings.autoRefreshSeconds * 1000);
}

function assertSession() {
  if (!state.session?.token) {
    const error = new Error("Not authenticated.");
    error.status = 401;
    throw error;
  }
}

async function api(path, init = {}) {
  assertSession();
  const response = await fetch(`${state.config.apiBaseUrl}${path}`, {
    ...init,
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${state.session.token}`,
      ...(init.headers || {}),
    },
  });
  const payloadText = await response.text();
  let payload = null;
  if (payloadText) {
    try {
      payload = JSON.parse(payloadText);
    } catch (_error) {
      payload = payloadText;
    }
  }
  if (!response.ok) {
    const error = new Error(
      typeof payload === "string"
        ? payload
        : payload?.message || payload?.error || `Request failed with status ${response.status}`
    );
    error.status = response.status;
    throw error;
  }
  return payload;
}

async function fetchJson(path) {
  const response = await fetch(path);
  if (!response.ok) {
    throw new Error(`Failed to load ${path}`);
  }
  return response.json();
}

async function copyText(value) {
  await navigator.clipboard.writeText(String(value || ""));
}

function downloadJson(filename, payload) {
  const blob = new Blob([JSON.stringify(payload, null, 2)], {
    type: "application/json;charset=utf-8",
  });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  URL.revokeObjectURL(url);
}

function loadSession() {
  return null;
}

function persistSession(_session) {}

function clearSession() {
  state.session = null;
  state.dashboard = null;
  state.agents = [];
  state.environments = [];
  state.traces = [];
  state.logs = [];
  state.alerts = [];
  state.alertRules = [];
  state.alertHistory = [];
  state.hitl = [];
  state.billing = [];
  state.billingUsage = [];
  state.audit = [];
  state.evaluations = [];
  state.annotationQueues = [];
  state.teamMembers = [];
  state.catalogTemplates = [];
  state.apiExplorer = { endpoints: [], apiKeys: [], openapiUrl: "" };
  state.secrets = [];
  state.selectedAgent = null;
  state.agentDetail = null;
  state.agentStatus = null;
  state.agentHistory = [];
  state.latestApiKeyToken = null;
  scheduleAutoRefresh();
}

function loadSettings() {
  try {
    const raw = localStorage.getItem("adk-deploy-settings");
    return raw ? { ...DEFAULT_SETTINGS, ...JSON.parse(raw) } : { ...DEFAULT_SETTINGS };
  } catch (_error) {
    return { ...DEFAULT_SETTINGS };
  }
}

function persistSettings(settings) {
  localStorage.setItem("adk-deploy-settings", JSON.stringify(settings));
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

function escapeAttribute(value) {
  return escapeHtml(value).replaceAll("\n", " ");
}
