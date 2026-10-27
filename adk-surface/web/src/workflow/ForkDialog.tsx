import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { ForkOutcome, ForkRequest, StepDetail } from "../generated/workflow";
import { ApiError, problemText } from "../client";
import { decisionPath } from "../route";
import { visible, visibleJson } from "../safe";
import { canFork, check } from "./fork";
import { workflowLabels } from "./labels";

/** The JSON text an editor starts from: the value as the step shows it. */
function textOf(value: unknown): string {
  return JSON.stringify(value, null, 2) ?? "null";
}

/**
 * The fork dialog (spec 3.3 §12): what a fork from this step runs again, which calls already
 * happened and are answered from their record unless changed, what becomes of a changed call,
 * one editor per channel, and "Start the fork now". Esc closes it without forking; the original
 * run is never written. An editor starts from the value as shown: a hidden or redacted part
 * keeps the value it stands for on the server, and a value too large to show is not edited.
 */
export function ForkDialog({
  detail,
  channels,
  waitingDecision,
  onFork,
  onClose,
}: {
  detail: StepDetail;
  channels: readonly string[];
  /** The decision of the session that waits, which refuses a fork until it is answered. */
  waitingDecision?: string;
  onFork: (request: ForkRequest) => Promise<ForkOutcome>;
  onClose: () => void;
}) {
  const title = useId();
  const dialog = useRef<HTMLDivElement>(null);
  const [edits, setEdits] = useState<Record<string, string>>({});
  const [open, setOpen] = useState<Record<string, boolean>>({});
  const [start, setStart] = useState(true);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<{ text: string; decision?: string }>();
  const checked = useMemo(() => check(edits, channels), [edits, channels]);
  const refusal = detail.fork.refusal ?? undefined;

  useEffect(() => {
    dialog.current?.focus();
    const close = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [onClose]);

  async function submit() {
    setBusy(true);
    setProblem(undefined);
    try {
      await onFork({ from: detail.checkpointId, set: checked.set, start });
    } catch (error) {
      if (error instanceof ApiError && error.code === "workflow.decision_waiting") {
        setProblem({ text: error.message, decision: waitingDecision });
      } else if (error instanceof ApiError && error.code === "fork.undeclared_channel") {
        setProblem({ text: workflowLabels.notAChannel(error.message.replace(/^.*: /, "")) });
      } else {
        setProblem({ text: workflowLabels.couldntFork(problemText(error)) });
      }
      setBusy(false);
    }
  }

  const blocked = refusal !== undefined || waitingDecision !== undefined;
  return (
    <div className="modal-scrim">
      <div className="fork-dialog" role="dialog" aria-modal="true" aria-labelledby={title} tabIndex={-1} ref={dialog}>
        <h2 id={title}>{workflowLabels.forkFrom(detail.step)}</h2>
        {refusal ? (
          <p className="notice">{visible(refusal)}</p>
        ) : waitingDecision ? (
          <p className="notice notice-problem">
            {workflowLabels.answerWaitingFirst}{" "}
            <a className="link" href={decisionPath(waitingDecision)}>
              {workflowLabels.openInDecisions}
            </a>
          </p>
        ) : (
          <>
            <p className="muted">{workflowLabels.forkExplainer(detail.step)}</p>
            <h3 className="card-label">{workflowLabels.whatHappens}</h3>
            <ul className="fork-what">
              {detail.fork.runsAgain.length > 0 && <li>{workflowLabels.runsAgain(detail.fork.runsAgain.map(visible))}</li>}
              {detail.fork.alreadyHappened.length > 0 && (
                <li>{workflowLabels.alreadyHappened(detail.fork.alreadyHappened.map((effect) => visible(effect.text)))}</li>
              )}
              {detail.fork.changedCalls && <li>{visible(detail.fork.changedCalls)}</li>}
            </ul>
            <h3 className="card-label">{workflowLabels.stateAt(detail.step)}</h3>
            <div className="fork-editor">
              {detail.stateValues.map((channel) => {
                const edited = edits[channel.key] !== undefined;
                const error = checked.errors[channel.key];
                return (
                  <div key={channel.key} className="fork-row" data-changed={edited}>
                    <div className="fork-key">
                      <span className="mono">{visible(channel.key)}</span>
                      {edited && <span className="change-word change-changed">{workflowLabels.changed}</span>}
                    </div>
                    <div className="fork-value">
                      {open[channel.key] ? (
                        <textarea
                          className="mono"
                          aria-label={channel.key}
                          aria-invalid={error !== undefined}
                          value={edits[channel.key] ?? textOf(channel.value)}
                          disabled={busy}
                          onChange={(event) => setEdits((all) => ({ ...all, [channel.key]: event.target.value }))}
                        />
                      ) : (
                        <span className="mono fork-current">{visibleJson(edits[channel.key] ?? JSON.stringify(channel.value) ?? "null")}</span>
                      )}
                      {channel.note && <p className="muted fork-note">{visible(channel.note)}</p>}
                      {error && <p className="field-error">{error}</p>}
                    </div>
                    <div className="fork-row-actions">
                      {edited && (
                        <button
                          type="button"
                          className="button button-quiet"
                          disabled={busy}
                          onClick={() => {
                            setEdits(({ [channel.key]: _, ...rest }) => rest);
                            setOpen((all) => ({ ...all, [channel.key]: false }));
                          }}
                        >
                          {workflowLabels.reset}
                        </button>
                      )}
                      <button
                        type="button"
                        className="button button-secondary"
                        disabled={busy || !channel.editable}
                        aria-pressed={open[channel.key] ?? false}
                        onClick={() => setOpen((all) => ({ ...all, [channel.key]: !all[channel.key] }))}
                      >
                        {workflowLabels.edit}
                      </button>
                    </div>
                  </div>
                );
              })}
            </div>
          </>
        )}
        {checked.notAChannel !== undefined && (
          <p className="notice notice-problem" role="alert">
            {workflowLabels.notAChannel(visible(checked.notAChannel))}
          </p>
        )}
        {problem && (
          <p className="notice notice-problem" role="alert">
            {visible(problem.text)}
            {problem.decision && (
              <>
                {" "}
                <a className="link" href={decisionPath(problem.decision)}>
                  {workflowLabels.openInDecisions}
                </a>
              </>
            )}
          </p>
        )}
        <label className="fork-start">
          <input type="checkbox" checked={start} disabled={busy} onChange={(event) => setStart(event.target.checked)} />
          {workflowLabels.startNow}
        </label>
        <div className="fork-actions">
          <button
            type="button"
            className="button button-primary"
            disabled={busy || blocked || !canFork(checked)}
            onClick={() => void submit()}
          >
            {busy ? workflowLabels.forking : workflowLabels.fork}
          </button>
          <button type="button" className="button button-quiet" onClick={onClose}>
            {workflowLabels.cancel}
          </button>
        </div>
      </div>
    </div>
  );
}
