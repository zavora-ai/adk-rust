import { useEffect, useId, useRef, useState } from "react";
import type { AnswerOption, CommandOutcome, DecisionCard } from "../generated/api";
import { AgentCard } from "../agents/AgentCard";
import { Avatar } from "../components/Avatar";
import { DECIDED_CLASS, DecidedLine } from "../components/DecidedLine";
import { PhoneIcon as Icon } from "./PhoneIcon";
import { Pill } from "../components/Pill";
import { ApiError, problemText } from "../client";
import { enterNeverDecides } from "../origin";
import { visible, visibleJson } from "../safe";
import type { PhoneClient } from "./client";
import { decidedLine } from "./Decisions";
import { phoneLabels } from "./labels";

/** The refusals that mean this phone cannot confirm it is the person (spec 3.3 §8.2). */
const PASSKEY_FAILED = new Set([
  "surface.phone.passkey_not_used",
  "surface.phone.no_passkey",
  "surface.phone.passkey_unavailable",
  "surface.phone.passkey_refused",
]);

/** The heading in two tones: the last word in the brand colour (spec 3.3 §9). */
function TwoTone({ text }: { text: string }) {
  const at = text.trimEnd().lastIndexOf(" ");
  if (at < 0) return <>{text}</>;
  return (
    <>
      {text.slice(0, at + 1)}
      <span className="phone-brand">{text.slice(at + 1)}</span>
    </>
  );
}

/**
 * One decision on the phone (spec 3.3 §5.4, §7.1, §8.2): who asks and since when, what will
 * happen, the exact command in full, how it got here, and the answers the phone may give. An
 * answer that needs presence goes through "Confirm it's you" and never the plain answer route.
 * The screen follows the session's feed: answered elsewhere, it shows who answered; changed,
 * it shows the new card and needs a new tap. Enter and Esc never decide. Decided, here or
 * elsewhere, the card is one line that opens to show the command again.
 *
 * A phone without its passkey, or whose passkey check fails (an installed iOS home-screen app
 * may refuse WebAuthn), offers only the answers that need no presence and says to approve in
 * the home on the computer: it never approves without presence.
 */
export function Decide({
  client,
  id,
  onBack,
  onSettled,
}: {
  client: PhoneClient;
  id: string;
  onBack: () => void;
  onSettled: (card: DecisionCard) => void;
}) {
  const [card, setCard] = useState<DecisionCard>();
  const [expanded, setExpanded] = useState(false);
  const [editing, setEditing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const [note, setNote] = useState("");
  const [inTheHome, setInTheHome] = useState(false);
  // The answer was given on this screen: the line takes the focus the answer buttons had.
  const [answeredHere, setAnsweredHere] = useState(false);
  const decidedHeading = useId();
  const settledRef = useRef(onSettled);
  settledRef.current = onSettled;

  useEffect(() => {
    let live = true;
    const read = async () => {
      try {
        const next = await client.decision(id);
        if (!live) return;
        setCard((current) => (current && current.status === "waiting" && next.status === "waiting" ? current : next));
        if (next.status === "decided") settledRef.current(next);
      } catch (error) {
        if (live) setProblem(problemText(error));
      }
    };
    void read();
    const timer = window.setInterval(() => {
      if (document.visibilityState !== "hidden") void read();
    }, 5_000);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, [client, id]);

  useEffect(() => {
    let live = true;
    void client
      .info()
      .then((info) => live && info.passkey === false && setInTheHome(true))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [client]);

  const session = card?.sessionId;
  useEffect(() => {
    if (!session) return;
    const abort = new AbortController();
    void (async () => {
      try {
        for await (const frame of client.watch(session, abort.signal)) {
          if (frame.type !== "decisions" || frame.cards.some((open) => open.id === id)) continue;
          // No longer open: someone answered it, here or elsewhere.
          const next = await client.decision(id);
          setCard(next);
          settledRef.current(next);
        }
      } catch {
        // The poll above keeps the screen current without the feed.
      }
    })();
    return () => abort.abort();
  }, [client, id, session]);

  if (!card) {
    return (
      <section className="phone-decide">
        <button type="button" className="phone-back" onClick={onBack}>
          <Icon name="back" /> {phoneLabels.back}
        </button>
        {problem && <p role="alert">{problem}</p>}
      </section>
    );
  }

  const decided = card.status === "decided";
  const shownInFull = !card.call.truncated || expanded;
  // A suggested agent shows its definition in place of the command (spec 3.3 §5.7).
  const addsAgent = card.addsAgent;

  function settle(outcome: CommandOutcome) {
    if (outcome.type === "answered" || outcome.type === "alreadyDecided") {
      setCard(outcome.card);
      setEditing(false);
      setAnsweredHere(true);
      settledRef.current(outcome.card);
    } else if (outcome.type === "changed") {
      setCard(outcome.card);
      setEditing(false);
      setNote(phoneLabels.changedSinceSeen);
    }
  }

  async function give(option: AnswerOption, editedArgs?: unknown) {
    if (!card) return;
    setBusy(true);
    setProblem("");
    setNote("");
    try {
      settle(
        option.needsPresence
          ? await client.confirm(card, option.answer, shownInFull, editedArgs)
          : await client.answer(card, option.answer, shownInFull),
      );
    } catch (error) {
      setProblem(problemText(error));
      if (option.needsPresence && error instanceof ApiError && PASSKEY_FAILED.has(error.code ?? "")) {
        setInTheHome(true);
      }
    } finally {
      setBusy(false);
    }
  }

  const offered = card.allowed.options.filter((option) => !inTheHome || !option.needsPresence);
  const presence = offered.find((option) => option.needsPresence && option.answer === "allowOnce");
  const others = offered.filter((option) => option !== presence);

  const who = (
    <header className="phone-decide-who">
      <Avatar agent={card.agent} size="md" />
      <span>
        <strong>{visible(card.agent.displayName)}</strong> · {card.sinceText}
      </span>
      <Pill kind={decided ? "decided" : card.risk === "high" ? "waiting" : "neutral"} dot={!decided && card.risk === "high"}>
        {decided ? phoneLabels.decided : card.riskText}
      </Pill>
    </header>
  );
  const lede = addsAgent ? null : (
    <p className="phone-lede">
      {phoneLabels.wantsToRun(visible(card.agent.displayName))} {card.reversibilityText}.
    </p>
  );
  const command = addsAgent ? (
    <AgentCard view={addsAgent} decisionId={card.id} expanded={expanded} onExpand={() => setExpanded(true)} decided={decided} changeable={false} />
  ) : (
    <figure className="phone-command">
      <figcaption>
        <i aria-hidden="true" className="phone-lights" /> {phoneLabels.theExactCommand}
      </figcaption>
      <pre>
        <code>{visible(shownInFull ? card.call.text : card.call.text)}</code>
      </pre>
      {card.call.truncated && !expanded && (
        <button type="button" className="phone-link" onClick={() => setExpanded(true)}>
          {phoneLabels.showFullCommand}
        </button>
      )}
    </figure>
  );
  const timeline = (
    <ol className="phone-timeline">
      {card.timeline.map((step, index) => (
        <li key={index} className={step.current ? "current" : undefined} aria-current={step.current ? "step" : undefined}>
          {visible(step.text)}
        </li>
      ))}
    </ol>
  );

  if (decided) {
    return (
      <section className="phone-decide phone-decide-decided" aria-labelledby={decidedHeading}>
        <button type="button" className="phone-back" onClick={onBack}>
          <Icon name="back" /> {phoneLabels.back}
        </button>
        <article className={DECIDED_CLASS} aria-labelledby={decidedHeading}>
          <DecidedLine card={card} headingId={decidedHeading} said={decidedLine(card)} focus={answeredHere}>
            {who}
            {lede}
            {command}
            {timeline}
          </DecidedLine>
        </article>
      </section>
    );
  }

  return (
    <section className="phone-decide" aria-labelledby="phone-decide-heading">
      <button type="button" className="phone-back" onClick={onBack}>
        <Icon name="back" /> {phoneLabels.back}
      </button>
      {who}
      <h1 id="phone-decide-heading" className="phone-display">
        <TwoTone text={visible(card.heading)} />
      </h1>
      {lede}
      {command}
      {timeline}
      {card.allowed.note && <p className="phone-note">{visible(card.allowed.note)}</p>}
      {inTheHome && offered.length < card.allowed.options.length && (
        <p className="phone-note">{phoneLabels.approveItInTheHome}</p>
      )}
      {note && (
        <p className="phone-note" role="status">
          {note}
        </p>
      )}
      {editing && (
        <EditArgs
          card={card}
          busy={busy}
          onCancel={() => setEditing(false)}
          onAllow={(args) => {
            const option = card.allowed.options.find((candidate) => candidate.answer === "edit");
            if (option) void give(option, args);
          }}
        />
      )}
      {problem && (
        <p className="phone-problem" role="alert">
          {problem}
        </p>
      )}
      {!editing && offered.length > 0 && (
        <footer className="phone-actions">
          {presence && (
            <button
              type="button"
              className="phone-button phone-button-primary"
              disabled={busy || (presence.needsExpanded && !shownInFull)}
              onKeyDown={enterNeverDecides}
              onClick={() => void give(presence)}
            >
              <Icon name="lock" /> {presence.label}
            </button>
          )}
          <div className="phone-actions-row">
            {others.map((option) => (
              <button
                key={option.answer}
                type="button"
                className={`phone-button ${option.answer === "allowOnce" ? "phone-button-primary" : "phone-button-secondary"}`}
                disabled={busy || (option.needsExpanded && !shownInFull)}
                onKeyDown={enterNeverDecides}
                onClick={() => (option.answer === "edit" ? setEditing(true) : void give(option))}
              >
                {option.label}
              </button>
            ))}
          </div>
          <p className="phone-footnote">{phoneLabels.answeredElsewhereUpdates}</p>
        </footer>
      )}
    </section>
  );
}

/** "Edit": change the arguments, then allow — after confirming it is you on high risk. */
function EditArgs({
  card,
  busy,
  onCancel,
  onAllow,
}: {
  card: DecisionCard;
  busy: boolean;
  onCancel: () => void;
  onAllow: (args: unknown) => void;
}) {
  const [text, setText] = useState(() => visibleJson(JSON.stringify(card.call.args ?? {}, null, 2)));
  const [invalid, setInvalid] = useState(false);
  const option = card.allowed.options.find((candidate) => candidate.answer === "edit");
  return (
    <div
      className="phone-edit"
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          onCancel();
        }
      }}
    >
      <textarea
        aria-label={option?.label ?? "Edit"}
        className="phone-edit-area"
        spellCheck={false}
        value={text}
        rows={Math.min(12, text.split("\n").length + 1)}
        onChange={(event) => {
          setText(event.target.value);
          setInvalid(false);
        }}
      />
      {invalid && <p role="alert">The arguments are not valid JSON.</p>}
      <div className="phone-actions-row">
        <button
          type="button"
          className="phone-button phone-button-primary"
          disabled={busy}
          onKeyDown={enterNeverDecides}
          onClick={() => {
            try {
              onAllow(JSON.parse(text));
            } catch {
              setInvalid(true);
            }
          }}
        >
          {option?.needsPresence ? phoneLabels.confirmItsYou : option?.label ?? "Edit"}
        </button>
        <button type="button" className="phone-button phone-button-secondary" onClick={onCancel}>
          Cancel
        </button>
      </div>
    </div>
  );
}
