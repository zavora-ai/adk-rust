/**
 * The developer panel's words for agent-drawn UI (spec 3.3 §4, §6.1 `genui`). Technical words
 * are allowed in this panel; the thread's words come from the engine (`generated/words.ts`).
 */
export const genuiLabels = {
  heading: "Agent-drawn UI",
  route: "GET /api/surface/genui/capabilities",
  claim: "Claim",
  status: "Status",
  scope: "Scope",
  catalogs: "Catalogs this home renders",
  leftOut: "Left out:",
  refused: "Refused drawings (this session)",
  refusedCount: (count: number) => `${count} refused`,
  noRefused: "Nothing refused in this session.",
  openThread: "Open a thread to see what was refused in it.",
  disabled: "Agent-drawn UI is not enabled on this server.",
  cantLoad: "Couldn't load the capabilities.",
  retry: "Retry",
  explainer: "Technical reasons stay in this panel. The thread shows a plain notice.",
  statuses: {
    supported: "Supported",
    partial: "Partial",
    host_owned: "Host-owned",
    candidate: "Candidate",
    planned: "Planned",
    legacy: "Legacy",
  },
  scopes: {
    runtime: "Runtime",
    client: "Client",
    transport: "Transport",
    host: "Host",
    security: "Security",
  },
} as const;
