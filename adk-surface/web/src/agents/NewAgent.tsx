import { useEffect, useId, useMemo, useState } from "react";
import type { MouseEvent } from "react";
import type { Origin } from "../generated/api";
import { problemText } from "../client";
import { originOf } from "../origin";
import { AgentForm, emptyDraft } from "./AgentForm";
import type { AgentsSeam } from "./client";
import { agentLabels as l } from "./labels";
import type { AgentChoices, AgentCommand, AgentDetail, AgentDraft, AgentOutcome } from "./types";
import { usePreview } from "./usePreview";
import { WhatItCanDo } from "./WhatItCanDo";

export type NewAgentTab = "describe" | "setUp";

/**
 * Makes a high-risk change after the person confirms it is them (spec 3.3 §7.2): the passkey on
 * this computer, bound by the server to exactly this change.
 */
export type ConfirmPresence = (command: AgentCommand) => Promise<AgentOutcome>;

/**
 * The New agent screen (`/ui/agents/new`, spec 3.3 §5.7): **Describe it** asks ADK-Rust to
 * suggest one in a new thread; **Set it up yourself** is the definition's form beside **Here is
 * what Finance can do**, kept current, then **Add Finance** with the preview's digest. Nothing
 * is kept until the person adds it. Opened from **Change it first**, the form holds the
 * suggestion and adding it ends the suggestion as "You changed it and added it".
 */
export function NewAgent({
  seam,
  primaryName = "ADK-Rust",
  initial,
  replaces,
  initialTab = "describe",
  confirm,
  onDescribe,
  onAdded,
  onCancel,
}: {
  seam: AgentsSeam;
  primaryName?: string;
  initial?: AgentDraft;
  replaces?: string;
  initialTab?: NewAgentTab;
  confirm?: ConfirmPresence;
  onDescribe: (text: string, origin: Origin) => Promise<void> | void;
  onAdded: (agent: AgentDetail) => void;
  onCancel: () => void;
}) {
  const [tab, setTab] = useState<NewAgentTab>(initial ? "setUp" : initialTab);
  const [choices, setChoices] = useState<AgentChoices>();
  const [draft, setDraft] = useState<AgentDraft | undefined>(initial);
  const [described, setDescribed] = useState("");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const describeId = useId();

  useEffect(() => {
    let live = true;
    seam
      .choices()
      .then((next) => {
        if (!live) return;
        setChoices(next);
        setDraft((current) => current ?? emptyDraft(next.areas[0]?.id ?? "default", next.colors[1]?.color ?? "green"));
      })
      .catch((error: unknown) => live && setProblem(problemText(error)));
    return () => {
      live = false;
    };
  }, [seam]);

  const request = useMemo(
    () => (tab === "setUp" && draft && draft.displayName.trim() ? { action: "create" as const, draft, replaces } : undefined),
    [tab, draft, replaces],
  );
  const { preview, problem: previewProblem, pending, current } = usePreview(seam, request);
  const name = draft?.displayName.trim() ?? "";

  async function add(event: MouseEvent<HTMLButtonElement>) {
    if (!draft || !preview) return;
    const origin = originOf(event);
    setBusy(true);
    setProblem("");
    try {
      const command: AgentCommand = { type: "addAgent", draft, digest: preview.digest, replaces };
      if (preview.needsPresence && !confirm) {
        setProblem(l.presenceNeeded);
        return;
      }
      const outcome =
        preview.needsPresence && confirm ? await confirm(command) : await seam.command(command, origin);
      if (outcome.type === "agentAdded") onAdded(outcome.agent);
    } catch (error) {
      setProblem(problemText(error));
    } finally {
      setBusy(false);
    }
  }

  async function describe(event: MouseEvent<HTMLButtonElement>) {
    const origin = originOf(event);
    setBusy(true);
    setProblem("");
    try {
      await onDescribe(`${l.suggestPrefix}${described.trim()}`, origin);
    } catch (error) {
      setProblem(problemText(error));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="agents-screen" aria-labelledby="new-agent-heading">
      <h1 id="new-agent-heading" className="display agents-title">
        {l.newAgent}
      </h1>
      <div className="tabs agent-tabs" role="tablist" aria-label={l.newAgent}>
        {(
          [
            ["describe", l.describeIt],
            ["setUp", l.setItUpYourself],
          ] as const
        ).map(([value, label]) => (
          <button
            key={value}
            type="button"
            role="tab"
            id={`agent-tab-${value}`}
            aria-selected={tab === value}
            aria-controls={`agent-panel-${value}`}
            className="tab"
            onClick={() => setTab(value)}
          >
            {label}
          </button>
        ))}
      </div>
      {tab === "describe" ? (
        <div id="agent-panel-describe" role="tabpanel" aria-labelledby="agent-tab-describe" className="card agent-describe">
          <label htmlFor={describeId} className="agent-label">
            {l.whatShouldItDo}
          </label>
          <textarea
            id={describeId}
            className="agent-input agent-textarea"
            rows={4}
            value={described}
            onChange={(event) => setDescribed(event.target.value)}
          />
          <p className="agent-hint">{l.describeHint(primaryName)}</p>
          <div className="agent-foot">
            <button
              type="button"
              className="button button-primary"
              disabled={busy || !described.trim()}
              onClick={(event) => void describe(event)}
            >
              {l.askToSuggestOne}
            </button>
            <button type="button" className="button button-quiet" onClick={onCancel}>
              {l.cancel}
            </button>
          </div>
        </div>
      ) : (
        <div id="agent-panel-setUp" role="tabpanel" aria-labelledby="agent-tab-setUp" className="agent-set-up">
          <div className="card">
            {choices && draft ? <AgentForm draft={draft} choices={choices} onChange={setDraft} disabled={busy} /> : null}
          </div>
          <div className="agent-side">
            <WhatItCanDo name={name} preview={name ? preview : undefined} problem={name ? previewProblem : l.emptyName} pending={pending} />
            {problem && (
              <p className="decision-problem" role="alert">
                {problem}
              </p>
            )}
            <div className="agent-foot">
              <button
                type="button"
                className="button button-primary"
                disabled={busy || !current || !name}
                onClick={(event) => void add(event)}
              >
                {preview?.needsPresence ? l.confirmItsYouAndAdd : l.addNamed(name)}
              </button>
              <button type="button" className="button button-quiet" onClick={onCancel}>
                {l.cancel}
              </button>
            </div>
          </div>
        </div>
      )}
      {tab === "describe" && problem && (
        <p className="decision-problem" role="alert">
          {problem}
        </p>
      )}
    </section>
  );
}
