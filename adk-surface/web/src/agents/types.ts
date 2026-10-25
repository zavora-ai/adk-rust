/**
 * The wire of agents a person adds (spec 3.3 §5.7), generated from adk-surface's `agents`
 * module. Every name and instruction is data, shown through `visible` (S-I9); an agent's `id`
 * is for commands only and is never shown (§8).
 */
import type { Command, CommandOutcome } from "../generated/api";

export type {
  AgentCardView,
  AgentChoices,
  AgentColor,
  AgentDetail,
  AgentDraft,
  AgentLife,
  AgentPreview,
  AgentPreviewRequest,
  AreaChoice,
  CardLine,
  ColorChoice,
  EndedWork,
  FileUse,
  ModelView,
  ShellUse,
  UsesDraft,
  WebUse,
} from "../generated/api";

type AgentCommandType =
  | "newThread"
  | "addAgent"
  | "changeAgent"
  | "pauseAgent"
  | "resumeAgent"
  | "removeAgent"
  | "copyRuleToAgent";

/** The engine's agent commands (spec 3.3 §6 "Commands"), sent through `POST /surface/commands`. */
export type AgentCommand = Extract<Command, { type: AgentCommandType }>;

/** What an agent command did. */
export type AgentOutcome = Extract<
  CommandOutcome,
  {
    type:
      | "sent"
      | "agentAdded"
      | "agentChanged"
      | "agentPaused"
      | "agentResumed"
      | "agentRemoved"
      | "agentOutOfDate"
      | "ruleCopied";
  }
>;
