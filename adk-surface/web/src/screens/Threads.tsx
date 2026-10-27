import { useEffect, useMemo, useRef, useState } from "react";
import type { AgentRow, DecisionCard, Origin, RemovedAgent, StatusLine } from "../generated/api";
import { agentLabels } from "../agents/labels";
import type { HomeClient } from "../client";
import { problemText } from "../client";
import { Avatar } from "../components/Avatar";
import { Composer } from "../components/Composer";
import type { Send, TakeOverSeam } from "../components/DecisionCard";
import { DecisionList } from "../components/DecisionList";
import { Roster } from "../components/Roster";
import { Thread } from "../components/Thread";
import { emptyThread, followFeed, wakeable } from "../feed";
import type { ThreadState } from "../feed";
import { briefingDue, briefingShown, quietWindows, rememberBriefing } from "../away";
import { dayAndTime, dollars, whenAhead } from "../format";
import type { AreaSettings, AwayPage } from "../kernel";
import { labels } from "../labels";
import type { Planned } from "../plan";
import type { VerbsResponse } from "../generated/verbs";
import { ComingUpRow } from "../verbs/Snooze";
import type { Snooze } from "../verbs/Snooze";

/** A session for an agent that has none yet: the engine creates it on the first message. */
function newSession() {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `s-${Date.now().toString(36)}`;
}

/**
 * Threads (spec 3.3 §5.1): the roster on the left, the selected agent's thread in the middle
 * with its decisions inline, and Decisions with a count badge and Coming up on the right.
 *
 * A thread follows one conversation (`followed`). The first time an agent is shown, that is
 * the session its roster row names, or a new one; from then on the thread stays on it, whatever
 * the row says later. A decision of another session is shown in that session's thread
 * (`visiting`) until the person goes back; the followed conversation never moves to it.
 */
export function Threads({
  client,
  agents,
  decisions,
  status,
  planned,
  send,
  onChanged,
  onToday,
  awayPage,
  areas,
  onDeveloper,
  takeOver,
  selected,
  onSelect,
  followed,
  onFollow,
  visiting,
  onBack,
  focusDecision,
  onOpenDecision,
  canAddAgents = false,
  removed = [],
  verbs,
  snooze,
}: {
  client: HomeClient;
  agents: AgentRow[];
  decisions: DecisionCard[];
  status?: StatusLine;
  planned: Planned[];
  send: Send;
  onChanged: () => void;
  onToday: () => void;
  /** The kernel's "While you were away", which the morning briefing shows the start of. */
  awayPage?: AwayPage;
  /** Each area's settings, for when quiet time ends. */
  areas?: AreaSettings[];
  onDeveloper: () => void;
  takeOver?: TakeOverSeam;
  /** The agent shown, once one is. */
  selected?: string;
  onSelect: (agent: string) => void;
  /** The session each agent's thread follows, by agent name ("" before any agent). */
  followed: Record<string, string>;
  onFollow: (agent: string, session: string) => void;
  /** A session of an agent shown in place of its conversation, from a decision opened. */
  visiting?: { agent: string; session: string };
  /** Back to the agent's own conversation. */
  onBack: () => void;
  /** The decision to bring into view. */
  focusDecision?: string;
  onOpenDecision: (card: DecisionCard) => void;
  /** The roster ends with New agent (spec 3.3 §5.7). */
  canAddAgents?: boolean;
  /** The agents the person removed: their threads open read only (spec 3.3 §5.7). */
  removed?: RemovedAgent[];
  /** What Coming up may be snoozed to, where the server takes screen actions (spec 3.3 §2 item 7). */
  verbs?: VerbsResponse;
  snooze?: Snooze;
}) {
  // A removed agent's thread, read only, in place of the selected agent's.
  const [reading, setReading] = useState<{ agent: RemovedAgent; session: string }>();
  const [fresh] = useState(newSession);
  const [queued, setQueued] = useState<{ session: string; items: string[] }>({ session: "", items: [] });
  const [sent, setSent] = useState(0);
  const [trouble, setTrouble] = useState(false);
  const [problem, setProblem] = useState("");
  const feed = useRef(wakeable());

  // Until the person picks one, the first agent waiting for them, else the first; once shown,
  // an agent stays selected when another starts waiting.
  const current = useMemo(() => {
    const chosen = agents.find((row) => row.agent.name === selected);
    return chosen ?? agents.find((row) => row.status === "waitingForYou") ?? agents[0];
  }, [agents, selected]);
  useEffect(() => {
    if (current && selected === undefined) onSelect(current.agent.name);
  }, [current, selected, onSelect]);

  const key = current?.agent.name ?? "";
  const pinned = followed[key];
  // The person's conversation with the agent, and the session shown: another one only while
  // a decision of it is being looked at.
  // An agent without a thread yet starts one of its own (spec 3.3 §5.7 "Threads").
  const own = pinned ?? current?.sessionId ?? (current?.added ? `${fresh}-${key}` : fresh);
  const away = visiting && visiting.agent === key && visiting.session !== own ? visiting.session : undefined;
  const session = reading?.session ?? away ?? own;
  // A decision opened elsewhere shows its own thread, not a removed agent's.
  useEffect(() => {
    if (visiting) setReading(undefined);
  }, [visiting]);
  useEffect(() => {
    if (!pinned) onFollow(key, own);
  }, [pinned, key, own, onFollow]);

  const [thread, setThread] = useState<ThreadState>(() => emptyThread(session));
  useEffect(() => {
    const abort = new AbortController();
    setThread(emptyThread(session));
    setTrouble(false);
    void followFeed(client, session, abort.signal, setThread, setTrouble, (ms) => feed.current.pause(ms));
    return () => abort.abort();
  }, [client, session]);

  // What waits behind an open decision is kept by the server, so a reload or a switch of
  // threads shows it again (spec 3.3 §7.4 rule 2); it is read whenever the thread moves.
  useEffect(() => {
    let live = true;
    client
      .queued(session)
      .then((items) => live && setQueued({ session, items }))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [client, session, thread.open, thread.entries.length, sent]);
  const shownQueued = queued.session === session ? queued.items : [];

  async function message(text: string, origin: Origin) {
    setProblem("");
    try {
      // The first message of an added agent's thread goes to that agent (spec 3.3 §5.7).
      const starts = current?.added === true && thread.entries.length === 0;
      await client.command(
        starts ? { type: "newThread", agent: key, sessionId: own, text } : { type: "send", sessionId: own, text },
        origin,
      );
      if (!pinned) onFollow(key, own);
      setSent((count) => count + 1);
      // A new session's first message shows now, not at the next look.
      feed.current.wake();
      onChanged();
    } catch (error) {
      setProblem(problemText(error));
      throw error;
    }
  }

  const name = reading ? reading.agent.agent.displayName : current?.agent.displayName;
  const shownAgent = reading ? reading.agent.agent : current?.agent;
  return (
    <div className="threads">
      <aside className="rail">
        <Roster
          agents={agents}
          selected={current?.agent.name}
          onSelect={(agent) => {
            setReading(undefined);
            onSelect(agent);
          }}
          canAdd={canAddAgents}
          removed={removed}
          reading={reading?.session}
          onRead={(agent, thread) => setReading({ agent, session: thread })}
        />
        <a
          className="rail-link"
          href="/ui/developer"
          onClick={(event) => {
            event.preventDefault();
            onDeveloper();
          }}
        >
          {labels.developer}
        </a>
      </aside>
      <main className="thread-pane" aria-label={name ?? labels.threads}>
        {/* The thread scrolls above the message box, which never covers it. */}
        <div className="thread-scroll">
          {shownAgent && (
            <header className="thread-head">
              <Avatar agent={shownAgent} size="lg" />
              <div>
                <h1 className="thread-name">{shownAgent.displayName}</h1>
                {reading ? (
                  <p className="thread-sub">{reading.agent.removedText}</p>
                ) : (
                  status?.mode && <p className="thread-sub">{status.mode.status}</p>
                )}
              </div>
            </header>
          )}
          {trouble && (
            <p className="trouble" role="status">
              {labels.cantReach}
            </p>
          )}
          <Thread
            key={session}
            entries={thread.session === session ? thread.entries : []}
            open={thread.session === session ? thread.open : []}
            client={client}
            send={send}
            onSettled={onChanged}
            focusDecision={focusDecision}
            takeOver={takeOver}
            session={session}
          />
          {problem && (
            <p className="decision-problem" role="alert">
              {problem}
            </p>
          )}
        </div>
        {reading ? (
          <p className="read-only-note" role="status">
            {agentLabels.wasRemoved(reading.agent.agent.displayName)}
          </p>
        ) : away ? (
          <div className="composer-wrap">
            <div className="visiting" role="status">
              <span>{labels.anotherConversation(name ?? labels.yourAgents)}</span>
              <button type="button" className="button button-secondary" onClick={onBack}>
                {labels.backToYourConversation}
              </button>
            </div>
          </div>
        ) : (
          <Composer
            placeholder={name ? labels.message(name) : labels.messageYourAgents}
            queued={shownQueued}
            disabled={status?.stopped}
            onSend={message}
          />
        )}
      </main>
      <aside className="side">
        <MorningBriefing away={awayPage} areas={areas} status={status} onToday={onToday} />
        <DecisionList decisions={decisions} onOpen={onOpenDecision} />
        <section className="side-section" aria-labelledby="coming-up-heading">
          <h2 id="coming-up-heading" className="side-heading">
            {labels.comingUp}
          </h2>
          {planned.length === 0 && <p className="side-empty">{labels.nothingComingUp}</p>}
          <ul className="coming-up">
            {planned.slice(0, 6).map((item) =>
              verbs ? (
                <ComingUpRow
                  key={item.key}
                  planned={item}
                  entry={verbs.comingUp.find((entry) => entry.id === item.item)}
                  snooze={snooze}
                  onSnoozed={onChanged}
                />
              ) : (
                <li key={item.key}>
                  <span>{item.agent ? `${item.agent} · ${item.title}` : item.title}</span>
                  <time className="mono" dateTime={item.at.toISOString()}>
                    {whenAhead(item.at)}
                  </time>
                </li>
              ),
            )}
          </ul>
          <a
            className="link link-accent"
            href="/ui/today"
            onClick={(event) => {
              event.preventDefault();
              onToday();
            }}
          >
            {labels.whileYouWereAway}
          </a>
        </section>
      </aside>
    </div>
  );
}

/**
 * The morning briefing (spec 3.3 §5.1, §12 phase 2): once quiet time ends, the start of "While
 * you were away" — what was spent, whether quiet time was kept, what waits and what happened —
 * from the same page Today reads. It shows once for each end of quiet time: **See what
 * happened** opens Today and **Not now** puts it away; neither marks anything seen.
 */
function MorningBriefing({
  away,
  areas,
  status,
  onToday,
}: {
  away?: AwayPage;
  areas?: AreaSettings[];
  status?: StatusLine;
  onToday: () => void;
}) {
  const [shown, setShown] = useState(briefingShown);
  const end = briefingDue(away, quietWindows(areas, status), new Date(), shown);
  if (!away || !end) return null;
  const putAway = () => {
    rememberBriefing(end);
    setShown(end.toISOString());
  };
  const counts = labels.countsLine(away.counts);
  return (
    <section className="briefing-card" aria-labelledby="morning-heading">
      <p className="briefing-card-since">{labels.since(dayAndTime(new Date(away.since)))}</p>
      <h2 id="morning-heading" className="side-heading">
        {labels.whileYouWereAway}
      </h2>
      <ul className="briefing-facts">
        <li>
          <strong>{dollars(away.spentMicroUsd)}</strong> {labels.spentWord}
        </li>
        <li>{away.quietTime.respected ? labels.quietRespectedLine : labels.quietNotKeptLine}</li>
        <li>{labels.waitingLine(away.waiting)}</li>
        {counts && <li className="briefing-counts">{counts}</li>}
      </ul>
      <div className="briefing-card-actions">
        <button
          type="button"
          className="button button-primary"
          onClick={() => {
            putAway();
            onToday();
          }}
        >
          {labels.seeWhatHappened}
        </button>
        <button type="button" className="button button-quiet" onClick={putAway}>
          {labels.notNow}
        </button>
      </div>
    </section>
  );
}
