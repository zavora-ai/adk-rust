import { useId } from "react";
import { Pill } from "../components/Pill";
import { words } from "../labels";
import { visible } from "../safe";
import { agentLabels as l } from "./labels";
import type { Mode } from "../generated/api";
import type { AgentPreview, GrantedUse } from "./types";

/**
 * **Here is what Finance can do** (spec 3.3 §5.7): the abilities, in plain words, and the risk
 * pill, kept current as the form changes. The words are the kernel's, from the preview. Under
 * them, "On this home it can also:" lists what the host grants every agent it runs in the mode
 * the form holds; the block is absent when it grants nothing there.
 */
export function WhatItCanDo({
  name,
  preview,
  problem,
  pending = false,
  granted = [],
  mode,
}: {
  name: string;
  preview?: AgentPreview;
  problem?: string;
  pending?: boolean;
  granted?: GrantedUse[];
  /** The mode the form holds: a use granted only in other modes is not listed. */
  mode?: Mode;
}) {
  const grantedId = useId();
  const uses = granted.filter((use) => !mode || !use.modes?.length || use.modes.includes(mode));
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
          {uses.length > 0 && (
            <div className="agent-granted">
              <p id={grantedId} className="agent-granted-heading">
                {words.ON_THIS_HOME_IT_CAN_ALSO}
              </p>
              <ul aria-labelledby={grantedId}>
                {uses.map((use) => (
                  <li key={use.id}>{visible(use.text)}</li>
                ))}
              </ul>
            </div>
          )}
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
