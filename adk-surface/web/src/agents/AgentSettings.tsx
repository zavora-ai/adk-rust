import { useCallback, useEffect, useId, useMemo, useState } from "react";
import type { MouseEvent } from "react";
import type { AgentRef, ModeView, Origin } from "../generated/api";
import { problemText } from "../client";
import { Avatar } from "../components/Avatar";
import { allows } from "../kernel";
import type { StandingRule } from "../kernel";
import { originOf } from "../origin";
import { visible } from "../safe";
import { AgentForm, MODES } from "./AgentForm";
import type { AgentsSeam } from "./client";
import { agentLabels as l } from "./labels";
import type { ConfirmPresence } from "./NewAgent";
import type { AgentChoices, AgentCommand, AgentDetail, AgentDraft, AgentPreview } from "./types";
import { usePreview } from "./usePreview";
import { WhatItCanDo } from "./WhatItCanDo";

/** A routine of the agent: a scheduled process of its target. */
export interface AgentRoutine {
  id: string;
  text: string;
  when: string;
}

/** A field of the definition in words, for "Was … Now …". */
export function fieldText(draft: AgentDraft, field: string): string {
  switch (field) {
    case "displayName":
      return draft.displayName;
    case "color":
      return draft.color;
    case "description":
      return draft.description || "—";
    case "instructions":
      return draft.instructions.length > 60 ? `${draft.instructions.slice(0, 60)}…` : draft.instructions || "—";
    case "mode":
      return MODES.find((mode) => mode.mode === draft.mode)?.label ?? draft.mode;
    case "model":
      return draft.model?.model ?? l.primaryModel;
    case "uses": {
      const uses = draft.uses;
      const files = uses.files === "none" ? l.files_.none : `${l.files_[uses.files]}${uses.folder ? ` (${uses.folder})` : ""}`;
      const web = uses.web.access === "listed" ? `${l.web_.listed}: ${uses.web.hosts.join(", ")}` : l.web_[uses.web.access];
      return [files, l.shell_[uses.shell], web, ...uses.packs, ...uses.drivers].join(", ");
    }
    case "dailyLimitMicroUsd":
      return draft.dailyLimitMicroUsd ? `$${(draft.dailyLimitMicroUsd / 1_000_000).toFixed(2)}` : "—";
    default:
      return "";
  }
}

const FIELD_LABELS: Record<string, string> = {
  displayName: l.name,
  color: l.colour,
  description: l.whatItDoes,
  instructions: l.instructions,
  mode: l.howMuchItAsksFirst,
  model: l.runsOn,
  uses: l.whatItCanUse,
  dailyLimitMicroUsd: l.dailyLimit,
};

/** "Was … Now …" for each field a change changes (spec 3.3 §5.7 "Changing an agent"). */
export function WasNow({ preview }: { preview: AgentPreview }) {
  if (!preview.was || !preview.now || !preview.fields?.length) return null;
  return (
    <dl className="agent-was-now" aria-label={`${l.was} · ${l.now}`}>
      {preview.fields.map((field) => (
        <div key={field} className="agent-was-now-row">
          <dt>{FIELD_LABELS[field] ?? field}</dt>
          <dd>
            <span className="muted">{l.was}</span> {visible(fieldText(preview.was!, field))}{" "}
            <span className="muted">{l.now}</span> {visible(fieldText(preview.now!, field))}
          </dd>
        </div>
      ))}
    </dl>
  );
}

/** Whether `rule` is pinned to the added agent `id`: its agent is exactly `urn:adk:agent:{app}/{id}` (S-I19). */
export function pinnedTo(rule: StandingRule, id: string): boolean {
  const agent = rule.agent ?? "";
  return agent.startsWith("urn:adk:agent:") && agent.slice(agent.lastIndexOf("/") + 1) === id;
}

function sameDraft(a: AgentDraft, b: AgentDraft): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/**
 * An agent's settings (spec 3.3 §5.7 "Agent settings"), from **Settings** on a roster row or
 * the thread head. For an agent a person added: how it was added, its definition with "Was …
 * Now …" before **Save changes**, **Copy one of your rules**, its routines with **Add a
 * routine**, and **Pause** and **Remove** at the foot, removing only after naming what ends.
 * The primary agent's settings show its mode and rules only.
 */
export function AgentSettings({
  seam,
  id,
  primary,
  routines = [],
  confirm,
  onAddRoutine,
  onChanged,
  onRemoved,
}: {
  seam: AgentsSeam;
  /** The agent a person added; none for the primary agent. */
  id?: string;
  /** The primary agent, with its mode, when `id` is none. */
  primary?: { agent: AgentRef; mode?: ModeView };
  routines?: AgentRoutine[];
  confirm?: ConfirmPresence;
  onAddRoutine?: (agent: string) => void;
  onChanged?: () => void;
  onRemoved?: (name: string) => void;
}) {
  const [detail, setDetail] = useState<AgentDetail>();
  const [choices, setChoices] = useState<AgentChoices>();
  const [draft, setDraft] = useState<AgentDraft>();
  const [rules, setRules] = useState<StandingRule[]>();
  const [chosenRule, setChosenRule] = useState("");
  const [removing, setRemoving] = useState<AgentPreview>();
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const [note, setNote] = useState("");
  const ruleId = useId();

  const load = useCallback(async () => {
    if (!id) return;
    const [next, options] = await Promise.all([seam.agent(id), seam.choices()]);
    setDetail(next);
    setChoices(options);
    setDraft(next.draft);
  }, [seam, id]);

  useEffect(() => {
    load().catch((error: unknown) => setProblem(problemText(error)));
    seam
      .rules?.()
      .then((all) => setRules((all ?? []).filter(allows)))
      .catch(() => setRules(undefined));
  }, [load, seam]);

  const changed = detail && draft && !sameDraft(detail.draft, draft);
  const request = useMemo(
    () => (detail && draft && changed ? { action: "change" as const, agent: detail.id, version: detail.version, draft } : undefined),
    [detail, draft, changed],
  );
  const { preview, problem: previewProblem, pending, current } = usePreview(seam, request);

  async function run(work: () => Promise<void>) {
    setBusy(true);
    setProblem("");
    setNote("");
    try {
      await work();
    } catch (error) {
      setProblem(problemText(error));
    } finally {
      setBusy(false);
    }
  }

  const save = (event: MouseEvent<HTMLButtonElement>) =>
    run(async () => {
      if (!detail || !draft || !preview) return;
      const origin = originOf(event);
      if (preview.needsPresence && !confirm) {
        setProblem(l.presenceNeeded);
        return;
      }
      const command: AgentCommand = {
        type: "changeAgent",
        agent: detail.id,
        version: detail.version,
        draft,
        digest: preview.digest,
      };
      const outcome =
        preview.needsPresence && confirm ? await confirm(command) : await seam.command(command, origin);
      if (outcome.type === "agentChanged") {
        setDetail(outcome.agent);
        setDraft(outcome.agent.draft);
        onChanged?.();
      } else if (outcome.type === "agentOutOfDate") {
        setDetail(outcome.agent);
        setDraft(outcome.agent.draft);
        setNote(outcome.text);
      }
    });

  const pauseOrResume = (event: MouseEvent<HTMLButtonElement>) =>
    run(async () => {
      if (!detail) return;
      const origin = originOf(event);
      const type = detail.state === "paused" ? "resumeAgent" : "pauseAgent";
      const outcome = await seam.command({ type, agent: detail.id }, origin);
      if (outcome.type === "agentPaused" || outcome.type === "agentResumed") {
        setDetail(outcome.agent);
        onChanged?.();
      }
    });

  const askRemove = () =>
    run(async () => {
      if (!detail) return;
      setRemoving(await seam.preview({ action: "remove", agent: detail.id, version: detail.version }));
    });

  const remove = (event: MouseEvent<HTMLButtonElement>) =>
    run(async () => {
      if (!detail || !removing) return;
      const origin = originOf(event);
      const outcome = await seam.command(
        { type: "removeAgent", agent: detail.id, version: detail.version, digest: removing.digest },
        origin,
      );
      if (outcome.type === "agentRemoved") onRemoved?.(detail.agent.displayName);
      else if (outcome.type === "agentOutOfDate") {
        setRemoving(undefined);
        setDetail(outcome.agent);
        setNote(outcome.text);
      }
    });

  const copy = (event: MouseEvent<HTMLButtonElement>, origin: Origin = originOf(event)) =>
    run(async () => {
      if (!detail || !chosenRule || !seam.copyRule) return;
      await seam.copyRule(detail.id, chosenRule, origin);
      const label = rules?.find((rule) => rule.id === chosenRule)?.label ?? "";
      setNote(l.copied(label));
      setChosenRule("");
      // The copy is the agent's own rule now.
      void seam
        .rules?.()
        .then((all) => setRules((all ?? []).filter(allows)))
        .catch(() => undefined);
    });

  // The agent's own rules, and the person's — pinned to no agent — that may be copied to it.
  const own = id ? rules?.filter((rule) => pinnedTo(rule, id)) : undefined;
  const copyable = rules?.filter((rule) => !rule.agent);

  if (!id) {
    // The primary agent: its mode and rules only (decision 17).
    return (
      <section className="card agent-settings" aria-labelledby="agent-settings-heading">
        {primary && (
          <header className="agent-settings-head">
            <Avatar agent={primary.agent} size="lg" />
            <h1 id="agent-settings-heading" className="card-heading">
              {l.settingsOf(visible(primary.agent.displayName))}
            </h1>
          </header>
        )}
        {primary?.mode && (
          <p className="setting">
            <span className="setting-label">{l.howMuchItAsksFirst}</span> <strong>{primary.mode.label}</strong>
          </p>
        )}
        <RuleList rules={copyable} />
        <p className="agent-hint">{l.primaryNote}</p>
      </section>
    );
  }

  const life = detail?.cantStart ? l.cantStart : detail?.state === "paused" ? l.paused : undefined;

  return (
    <section className="agent-settings-screen" aria-labelledby="agent-settings-heading">
      {detail && (
        <header className="agent-settings-head">
          <Avatar agent={detail.agent} size="lg" />
          <div>
            <h1 id="agent-settings-heading" className="display agents-title">
              {visible(detail.agent.displayName)}
            </h1>
            <p className="muted">
              {visible(detail.originText)}
              {life && <> · <strong>{life}</strong></>}
              {detail.cantStart && <> — {visible(detail.cantStart)}</>}
            </p>
          </div>
        </header>
      )}
      {note && (
        <p className="change-note" role="status">
          {note}
        </p>
      )}
      {problem && (
        <p className="decision-problem" role="alert">
          {problem}
        </p>
      )}
      {detail && draft && choices && (
        <div className="agent-set-up">
          <div className="card">
            <AgentForm draft={draft} choices={choices} onChange={setDraft} areaFixed disabled={busy} />
          </div>
          <div className="agent-side">
            {changed ? (
              <>
                <WhatItCanDo
                  name={draft.displayName}
                  preview={preview}
                  problem={previewProblem}
                  pending={pending}
                  granted={detail.granted ?? []}
                />
                {preview && <WasNow preview={preview} />}
                <div className="agent-foot">
                  <button
                    type="button"
                    className="button button-primary"
                    disabled={busy || !current}
                    onClick={(event) => void save(event)}
                  >
                    {preview?.needsPresence ? l.confirmItsYouAndSave : l.saveChanges}
                  </button>
                  <button type="button" className="button button-quiet" onClick={() => setDraft(detail.draft)}>
                    {l.cancel}
                  </button>
                </div>
              </>
            ) : null}
            <section className="card agent-section" aria-labelledby={ruleId}>
              <h2 id={ruleId} className="card-label">
                {l.allowedWithoutAsking}
              </h2>
              {own !== undefined && own.length > 0 ? (
                <ul className="bordered" aria-label={l.itsRules}>
                  {own.map((rule) => (
                    <li key={rule.id} className="rule">
                      {visible(rule.label)}
                    </li>
                  ))}
                </ul>
              ) : (
                <p className="muted">{l.nothingYet}</p>
              )}
              <p className="agent-hint">{l.allowedHint}</p>
              {seam.copyRule && copyable !== undefined && (
                copyable.length === 0 ? (
                  <p className="muted">{l.noRulesToCopy}</p>
                ) : (
                  <div className="agent-copy-rule">
                    <select
                      className="agent-input"
                      aria-label={l.yourRules}
                      value={chosenRule}
                      onChange={(event) => setChosenRule(event.target.value)}
                    >
                      <option value="">{l.chooseARule}</option>
                      {copyable.map((rule) => (
                        <option key={rule.id} value={rule.id}>
                          {rule.label}
                        </option>
                      ))}
                    </select>
                    <button
                      type="button"
                      className="button button-secondary"
                      disabled={busy || !chosenRule}
                      onClick={(event) => void copy(event)}
                    >
                      {l.copyOneOfYourRules}
                    </button>
                  </div>
                )
              )}
            </section>
            <section className="card agent-section" aria-label={l.routines}>
              <h2 className="card-label">{l.routines}</h2>
              {routines.length === 0 ? (
                <p className="muted">{l.noRoutines}</p>
              ) : (
                <ul className="bordered">
                  {routines.map((routine) => (
                    <li key={routine.id} className="agent-routine">
                      <span>{visible(routine.text)}</span>
                      <span className="mono muted">{routine.when}</span>
                    </li>
                  ))}
                </ul>
              )}
              {onAddRoutine && (
                <button type="button" className="button button-secondary" onClick={() => onAddRoutine(detail.id)}>
                  {l.addRoutine}
                </button>
              )}
            </section>
            {removing ? (
              <div className="card agent-remove" role="alertdialog" aria-labelledby="remove-heading" aria-describedby="remove-text">
                <h2 id="remove-heading" className="card-heading">
                  {l.removeQuestion(visible(detail.agent.displayName))}
                </h2>
                <p id="remove-text">{visible(removing.ended?.text ?? "")}</p>
                <div className="agent-foot">
                  <button type="button" className="button button-primary" disabled={busy} onClick={(event) => void remove(event)}>
                    {l.remove}
                  </button>
                  <button type="button" className="button button-secondary" disabled={busy} onClick={() => setRemoving(undefined)}>
                    {l.keep}
                  </button>
                </div>
              </div>
            ) : (
              <div className="agent-foot agent-danger">
                <button type="button" className="button button-secondary" disabled={busy} onClick={(event) => void pauseOrResume(event)}>
                  {detail.state === "paused" ? l.resume : l.pause}
                </button>
                <button type="button" className="button button-secondary" disabled={busy} onClick={() => void askRemove()}>
                  {l.remove}
                </button>
              </div>
            )}
          </div>
        </div>
      )}
    </section>
  );
}

function RuleList({ rules }: { rules?: StandingRule[] }) {
  if (rules === undefined) return null;
  return (
    <>
      <p className="card-label">{l.allowedWithoutAsking}</p>
      {rules.length === 0 ? (
        <p className="muted">{l.nothingYet}</p>
      ) : (
        <ul className="bordered">
          {rules.map((rule) => (
            <li key={rule.id} className="rule">
              {visible(rule.label)}
            </li>
          ))}
        </ul>
      )}
    </>
  );
}
