import { useEffect, useState } from "react";
import type { AgentRow, DecisionCard, ModeView, Origin } from "../generated/api";
import { problemText } from "../client";
import { AgentSettings } from "./AgentSettings";
import type { AgentRoutine } from "./AgentSettings";
import type { AgentsSeam } from "./client";
import { agentLabels as l } from "./labels";
import { NewAgent } from "./NewAgent";
import type { ConfirmPresence } from "./NewAgent";
import type { AgentsPlace } from "./route";
import { agentSettingsPath, goTo } from "./route";
import type { AgentCardView, AgentDraft } from "./types";
import "./agents.css";

/** The suggestion a decision holds, for **Change it first**. */
function suggestionOf(card: DecisionCard): AgentCardView | undefined {
  return (card as DecisionCard & { addsAgent?: AgentCardView | null }).addsAgent ?? undefined;
}

/**
 * The agents screens of the web home (spec 3.3 §5.7): New agent at `/ui/agents/new` and an
 * agent's settings at `/ui/agents/<id>`. The primary agent's settings show its mode and rules.
 */
export function AgentsScreen({
  place,
  seam,
  rows,
  primary,
  routinesOf,
  decision,
  confirm,
  onDescribe,
  onAddRoutine,
  onChanged,
  onDone,
}: {
  place: AgentsPlace;
  seam: AgentsSeam;
  rows: AgentRow[];
  /** The primary agent: its kernel target and mode. */
  primary?: { name: string; mode?: ModeView };
  routinesOf?: (agent: string) => AgentRoutine[];
  /** Reads a decision, for **Change it first**. */
  decision?: (id: string) => Promise<DecisionCard>;
  confirm?: ConfirmPresence;
  onDescribe: (text: string, origin: Origin) => Promise<void> | void;
  onAddRoutine?: (agent: string) => void;
  onChanged: () => void;
  onDone: (note?: string) => void;
}) {
  const [held, setHeld] = useState<{ draft?: AgentDraft; replaces?: string; problem?: string; loaded: boolean }>({
    loaded: false,
  });
  const from = place.kind === "new" ? place.from : undefined;

  useEffect(() => {
    if (!from || !decision) {
      setHeld({ loaded: true });
      return;
    }
    let live = true;
    decision(from)
      .then((card) => {
        const held = suggestionOf(card);
        if (live) setHeld({ draft: held?.draft, replaces: held?.id, loaded: true });
      })
      .catch((error: unknown) => live && setHeld({ problem: problemText(error), loaded: true }));
    return () => {
      live = false;
    };
  }, [from, decision]);

  if (place.kind === "new") {
    if (!held.loaded) return null;
    const primaryName = rows.find((row) => row.agent.name === primary?.name)?.agent.displayName ?? "ADK-Rust";
    return (
      <>
        {held.problem && (
          <p className="banner" role="alert">
            {held.problem}
          </p>
        )}
        <NewAgent
          key={from ?? "new"}
          seam={seam}
          primaryName={primaryName}
          initial={held.draft}
          replaces={held.replaces}
          confirm={confirm}
          onDescribe={onDescribe}
          onAdded={(agent) => {
            onChanged();
            goTo(agentSettingsPath(agent.id));
          }}
          onCancel={() => onDone()}
        />
      </>
    );
  }
  const row = rows.find((candidate) => candidate.agent.name === place.id);
  const isPrimary = primary !== undefined && place.id === primary.name;
  return (
    <>
      <button type="button" className="link agents-back" onClick={() => onDone()}>
        {l.back}
      </button>
      {isPrimary ? (
        <AgentSettings seam={seam} primary={row ? { agent: row.agent, mode: primary?.mode } : undefined} />
      ) : (
        <AgentSettings
          key={place.id}
          seam={seam}
          id={place.id}
          routines={routinesOf?.(place.id)}
          confirm={confirm}
          onAddRoutine={onAddRoutine}
          onChanged={onChanged}
          onRemoved={(name) => {
            onChanged();
            onDone(l.removedNote(name));
          }}
        />
      )}
    </>
  );
}
