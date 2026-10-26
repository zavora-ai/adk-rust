/**
 * How the web home reads and changes agents a person adds (spec 3.3 §5.7): the engine's reads
 * under `/api/surface/agents`, and every change through `POST /api/surface/commands`, which
 * closes them to paired devices. Every change previews first and applies only with the
 * preview's digest.
 */
import type { Origin } from "../generated/api";
import type { StandingRule } from "../kernel";
import type {
  AgentChoices,
  AgentCommand,
  AgentDetail,
  AgentOutcome,
  AgentPreview,
  AgentPreviewRequest,
} from "./types";

export interface AgentsSeam {
  /** One agent a person added. */
  agent(id: string): Promise<AgentDetail>;
  /** The models, packs, areas and colours a person may choose. */
  choices(): Promise<AgentChoices>;
  /** What a change would let an agent do, with the digest it applies with. */
  preview(request: AgentPreviewRequest): Promise<AgentPreview>;
  /** Carries out a change previewed first. */
  command(command: AgentCommand, origin: Origin): Promise<AgentOutcome>;
  /** The person's standing rules **Copy one of your rules** offers; none without a harness. */
  rules?(): Promise<StandingRule[] | undefined>;
  /** Copies the person's rule `rule` to `agent`, pinned to it (S-I19). */
  copyRule?(agent: string, rule: string, origin: Origin): Promise<void>;
}

/** The requests the seam makes, from `HttpClient`. */
export interface AgentsTransport {
  get<T>(path: string): Promise<T>;
  post<T>(path: string, body: unknown): Promise<T>;
  command(command: unknown, origin: Origin): Promise<unknown>;
  /** The person's standing rules; `undefined` without a harness. */
  rules?(): Promise<StandingRule[] | undefined>;
}

/** The seam over the engine's HTTP API. */
export function httpAgents(transport: AgentsTransport): AgentsSeam {
  return {
    agent: (id) => transport.get<AgentDetail>(`/surface/agents/${encodeURIComponent(id)}`),
    choices: () => transport.get<AgentChoices>("/surface/agents/choices"),
    preview: (request) => transport.post<AgentPreview>("/surface/agents/preview", request),
    command: async (command, origin) => (await transport.command(command, origin)) as AgentOutcome,
    copyRule: async (agent, rule, origin) => {
      await transport.command({ type: "copyRuleToAgent", agent, ruleId: rule }, origin);
    },
    // Settings show the agent's own rules and offer the person's to copy (spec 3.3 §5.7).
    ...(transport.rules ? { rules: transport.rules } : {}),
  };
}
