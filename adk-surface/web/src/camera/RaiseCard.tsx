import { useEffect, useId, useState } from "react";
import type { AgentRef, CardRaised, StageView } from "../generated/api";
import { Avatar } from "../components/Avatar";
import { problemText } from "../client";
import { CameraStage } from "./CameraStage";
import { cameraLabels as words } from "./labels";

/**
 * Raise a card from the current frame (board choice C11): the frame frozen, a note to the
 * agent, and what is kept — that one frame, 30 days, everyone in it blurred first, the regions on
 * screen with it. A message to the agent with evidence, not a decision.
 */
export function RaiseCard({
  frozen,
  name,
  agent,
  width = 520,
  initialNote = "",
  onRaise,
  onCancel,
}: {
  /** The frozen frame's picture, as the server holds it. */
  frozen: StageView;
  /** The camera's name. */
  name: string;
  agent: AgentRef;
  width?: number;
  initialNote?: string;
  onRaise: (note: string) => Promise<CardRaised>;
  onCancel: () => void;
}) {
  const [note, setNote] = useState(initialNote);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const field = useId();
  const at = frozen.frame?.atText ?? "";
  const regions = frozen.regions.filter((region) => region.bbox).length;

  useEffect(() => {
    const keys = (event: KeyboardEvent) => event.key === "Escape" && onCancel();
    window.addEventListener("keydown", keys);
    return () => window.removeEventListener("keydown", keys);
  }, [onCancel]);

  async function raise() {
    setBusy(true);
    setProblem("");
    try {
      await onRaise(note);
    } catch (error) {
      setProblem(problemText(error));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="cam-panel-card" aria-labelledby={`${field}-title`}>
      <header className="cam-panel-head">
        <h2 id={`${field}-title`}>{words.cardTitle}</h2>
        <span className="cam-muted">
          {name} · {at}
        </span>
      </header>
      <div className="cam-panel-body">
        <div className="cam-panel-picture">
          <CameraStage
            stage={{ ...frozen, badge: { text: words.frozen(at), tone: "plain" }, rate: undefined }}
            width={width}
            name={name}
            size="small"
          />
        </div>
        <div className="cam-panel-fields">
          <div className="cam-field">
            <span className="cam-label">{words.for}</span>
            <span className="cam-for">
              <Avatar agent={agent} size="sm" />
              {agent.displayName}
            </span>
          </div>
          <label className="cam-field" htmlFor={`${field}-note`}>
            <span className="cam-label">{words.lookAt}</span>
          </label>
          <textarea id={`${field}-note`} className="cam-input cam-textarea" value={note} rows={3} onChange={(event) => setNote(event.target.value)} />
          <dl className="cam-kv">
            <dt>{words.kept}</dt>
            <dd>{words.keptText}</dd>
            <dt>{words.beforeKept}</dt>
            <dd>{words.beforeKeptText}</dd>
            <dt>{words.regions}</dt>
            <dd>{words.regionsText(regions)}</dd>
          </dl>
          {problem && (
            <p className="cam-problem" role="alert">
              {problem}
            </p>
          )}
          <div className="cam-panel-actions">
            <button type="button" className="cam-btn cam-btn-primary cam-btn-sm" disabled={busy || !note.trim()} onClick={() => void raise()}>
              {words.raiseTheCard}
            </button>
            <button type="button" className="cam-btn cam-btn-text cam-btn-sm" onClick={onCancel}>
              {words.cancel}
            </button>
          </div>
        </div>
      </div>
    </section>
  );
}
