import { Pill } from "../components/Pill";
import { visible } from "../safe";
import { agentLabels as l } from "./labels";
import type { AgentPreview } from "./types";

/**
 * **Here is what Finance can do** (spec 3.3 §5.7): the abilities, in plain words, and the risk
 * pill, kept current as the form changes. The words are the kernel's, from the preview.
 */
export function WhatItCanDo({
  name,
  preview,
  problem,
  pending = false,
}: {
  name: string;
  preview?: AgentPreview;
  problem?: string;
  pending?: boolean;
}) {
  return (
    <aside className="agent-can-do" aria-live="polite" aria-busy={pending} aria-label={l.hereIsWhatCanDo(name)}>
      <h3 className="agent-can-do-heading">{visible(preview?.heading ?? l.hereIsWhatCanDo(name))}</h3>
      {problem ? (
        <p className="decision-problem" role="alert">
          {problem}
        </p>
      ) : preview ? (
        <>
          <ul className="agent-abilities">
            {preview.abilities.map((ability, index) => (
              <li key={index}>{visible(ability)}</li>
            ))}
          </ul>
          <p className="agent-risk">
            <Pill kind={preview.risk === "low" ? "neutral" : "waiting"}>{preview.riskText}</Pill>
            {preview.money && <span className="muted"> · {l.spendsMoney}</span>}
            <span className="muted"> · {l.youCanRemoveItLater}</span>
          </p>
        </>
      ) : null}
    </aside>
  );
}
