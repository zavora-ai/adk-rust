/**
 * Snooze on a Coming up row (spec 3.3 §2 item 7, §5.1): on hover or keyboard focus of an
 * agenda row that waits for one time, a "Snooze" button opens "1 hour later" and "Tomorrow,
 * same time". It acts only on a person's own click, tap or key, and says it was done only
 * once the server answered.
 */
import { useEffect, useId, useRef, useState } from "react";
import type { KeyboardEvent, MouseEvent } from "react";
import type { Origin } from "../generated/api";
import type { ComingUpItem, SnoozeChoice, VerbDone } from "../generated/verbs";
import { problemText } from "../client";
import { whenAhead } from "../format";
import { originOf } from "../origin";
import type { Planned } from "../plan";
import { verbLabels } from "./labels";
import { trusted } from "./trust";
import "./verbs.css";

export type Snooze = (item: ComingUpItem, choice: SnoozeChoice, origin: Origin) => Promise<VerbDone>;

type State = { kind: "idle" } | { kind: "snoozing" } | { kind: "done" } | { kind: "cant"; text: string };

/** One Coming up row, with Snooze when `entry` may be snoozed. */
export function ComingUpRow({
  planned,
  entry,
  snooze,
  onSnoozed,
}: {
  planned: Planned;
  /** The row's agenda item as the server lists it for screen actions. */
  entry?: ComingUpItem;
  snooze?: Snooze;
  onSnoozed?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [state, setState] = useState<State>({ kind: "idle" });
  const menu = useId();
  const first = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (open) first.current?.focus();
  }, [open]);
  const can = snooze !== undefined && entry !== undefined && !entry.whyNot && entry.choices.length > 0;

  async function choose(choice: SnoozeChoice, event: MouseEvent<HTMLButtonElement>) {
    // Only the person's own input acts, never a script's.
    if (!trusted(event) || !snooze || !entry) return;
    setOpen(false);
    setState({ kind: "snoozing" });
    try {
      await snooze(entry, choice, originOf(event));
      setState({ kind: "done" });
      onSnoozed?.();
    } catch (error) {
      setState({ kind: "cant", text: verbLabels.cantSnooze(problemText(error)) });
    }
  }

  function closeOnEscape(event: KeyboardEvent) {
    if (event.key === "Escape") setOpen(false);
  }

  const busy = state.kind === "snoozing";
  return (
    <li className={`coming-up-item${can ? " can-snooze" : ""}${open ? " menu-open" : ""}`} onKeyDown={closeOnEscape}>
      <div className="coming-up-row">
        <span>{planned.agent ? `${planned.agent} · ${planned.title}` : planned.title}</span>
        <span className="coming-up-end">
          <time className="mono" dateTime={planned.at.toISOString()}>
            {whenAhead(planned.at)}
          </time>
          {can && !busy && (
            <button
              type="button"
              className="snooze-button"
              aria-haspopup="menu"
              aria-expanded={open}
              aria-controls={menu}
              aria-label={verbLabels.menu(planned.title)}
              onClick={(event) => {
                if (trusted(event)) setOpen(!open);
              }}
            >
              {verbLabels.snooze}
            </button>
          )}
          {busy && (
            <span className="snoozing" role="status">
              {verbLabels.snoozing}
            </span>
          )}
        </span>
      </div>
      {open && entry && (
        <ul className="snooze-menu" role="menu" id={menu}>
          {entry.choices.map((choice, index) => (
            <li role="none" key={choice.to}>
              <button
                type="button"
                role="menuitem"
                ref={index === 0 ? first : undefined}
                onClick={(event) => void choose(choice, event)}
              >
                <span>{choice.label}</span>
                <time className="mono" dateTime={choice.to}>
                  {choice.atText}
                </time>
              </button>
            </li>
          ))}
        </ul>
      )}
      {state.kind === "done" && <p className="coming-up-note">{verbLabels.snoozedByYou}</p>}
      {state.kind === "cant" && (
        <p className="coming-up-note" role="status">
          {state.text}
        </p>
      )}
    </li>
  );
}
