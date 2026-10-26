import { useEffect, useId, useRef, useState } from "react";
import type { ReactNode } from "react";
import type { DecisionCard } from "../generated/api";
import { visible } from "../safe";
import "./decided-line.css";

/** The class of the box a `DecidedLine` sits in: the decided tint of the §9 tokens. */
export const DECIDED_CLASS = "decided";

/**
 * A decided card, collapsed to one line (spec 3.3 §5.1, §5.4, §5.5, §7.1): a check, the
 * heading, then the engine's words for who answered and when — "Write file tray-demo.txt ·
 * You said yes from the tray at 15:24". Where the line is too narrow for both, the answer
 * wraps under the heading rather than being cut. It collapses whichever surface answered. A click, Enter or
 * Space on the line shows `children` — the exact call and the details — and hides them
 * again; that is all it does, so no key and no click on it answers anything (§7.4).
 *
 * It goes inside the card's own element, which keeps its place: that element takes
 * [`DECIDED_CLASS`] and is labelled by `headingId`, the id the line gives the heading. Its look
 * comes from the §9 tokens in `decided-line.css`, so a surface that imports it — the web
 * home's thread, the tray panel, the phone — needs no other sheet.
 *
 * # Example
 *
 * ```tsx
 * const heading = useId();
 * <article className={DECIDED_CLASS} aria-labelledby={heading}>
 *   <DecidedLine card={card} headingId={heading} focus={answeredHere}>
 *     <code>{card.call.text}</code>
 *     <p>{card.reversibilityText}</p>
 *   </DecidedLine>
 * </article>
 * ```
 */
export function DecidedLine({
  card,
  headingId,
  said,
  focus = false,
  children,
}: {
  /** A card whose `status` is `"decided"`. */
  card: DecisionCard;
  /** The id of the heading, which labels the card's element. */
  headingId: string;
  /** Who answered and when, in the engine's words; `card.decided.text` when not given. */
  said?: string;
  /** Moves the keyboard focus to the line when it appears, as after an answer given here. */
  focus?: boolean;
  /** What the line shows when it is opened. */
  children?: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const details = useId();
  const line = useRef<HTMLButtonElement>(null);
  const heading = visible(card.heading);
  const answer = visible(said || card.decided?.text || card.statusText);

  // Only when the line appears: a later render never takes the focus back.
  const focused = useRef(focus);
  useEffect(() => {
    if (focused.current) line.current?.focus();
  }, []);

  return (
    <>
      <h3 className="decided-title">
        <button
          ref={line}
          type="button"
          className="decided-line"
          aria-expanded={open}
          aria-controls={details}
          title={`${heading} · ${answer}`}
          onClick={() => setOpen((shown) => !shown)}
        >
          <span className="decided-check" aria-hidden="true">
            <svg viewBox="0 0 24 24" focusable="false">
              <path d="M5 12.5 10 17.5 19 7" />
            </svg>
          </span>
          <span className="decided-words">
            <span id={headingId} className="decided-heading">
              {heading}
            </span>{" "}
            <span className="decided-dot" aria-hidden="true">
              ·
            </span>{" "}
            <span className="decided-text">{answer}</span>
          </span>
          <svg className="decided-chevron" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
            <path d="m6 9 6 6 6-6" />
          </svg>
        </button>
      </h3>
      <div id={details} className="decided-details" hidden={!open}>
        {open ? children : null}
      </div>
    </>
  );
}
