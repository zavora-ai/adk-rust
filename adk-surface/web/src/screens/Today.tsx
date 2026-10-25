import { useEffect, useId, useState } from "react";
import type { AgentRow, Briefing, BriefingLine, Pill as PillKindApi, StatusLine } from "../generated/api";
import type { HomeClient } from "../client";
import { problemText } from "../client";
import { agentOf as agentName, endToday, inOrder, itemSentence, lineWords, quietWindows } from "../away";
import type { LineWords } from "../away";
import { Pill } from "../components/Pill";
import type { PillKind } from "../components/Pill";
import { dayAndTime, dayMonth, dollars, hhmm, sameDay } from "../format";
import type { AgendaItem, AreaSettings, AwayLine, AwayPage, AwayRecord } from "../kernel";
import { agendaState, dailyCron, isQuestion } from "../kernel";
import { labels, words } from "../labels";
import type { Planned } from "../plan";
import { visible } from "../safe";
import { windowText } from "../settings";
import type { Seams, WhileAwaySeam } from "../seams";

const pillKind: Record<PillKindApi, PillKind> = {
  done: "done",
  failed: "failed",
  waitingForYou: "waiting",
  youDecided: "decided",
  suggestion: "suggestion",
  question: "question",
};

/** How far apart a suggestion's ledger line and its agenda item may be stamped. */
const SAME_EVENT_MS = 10_000;

/**
 * A line of the timeline: one of the kernel's "While you were away" (K4), one of the engine's
 * briefing where the kernel keeps no ledger, or an open suggestion or question.
 */
type Row =
  | { kind: "away"; key: string; at: Date; failed: boolean; line: AwayLine; said: LineWords; item?: AgendaItem }
  | { kind: "line"; key: string; at: Date; failed: boolean; line: BriefingLine }
  | { kind: "ask"; key: string; at: Date; failed: boolean; item: AgendaItem };

/**
 * Today — "While you were away" (spec 3.3 §5.1, §10 K4): what was spent, whether quiet time
 * was kept, how many decisions wait, then one line per event since the person last marked it
 * seen, each with a status pill, what can be done about it and its record, and today's
 * routines. Opening the page reads; only **Mark as seen** moves on.
 */
export function Today({
  client,
  agents,
  status,
  agenda,
  away,
  areas,
  routinesToday,
  canAddRoutine,
  seams,
  onOpenAgent,
  onChanged,
  onAwayChanged,
}: {
  client: HomeClient;
  agents: AgentRow[];
  status?: StatusLine;
  agenda?: AgendaItem[];
  /** The kernel's "While you were away", as the home last read it; `undefined` without one. */
  away?: AwayPage;
  /** Each area's settings, for quiet time; `undefined` where the kernel serves none. */
  areas?: AreaSettings[];
  /** Today's routines, past ones included. */
  routinesToday: Planned[];
  /** The kernel serves its process table, where a routine is a sleeping process. */
  canAddRoutine: boolean;
  seams: Seams;
  onOpenAgent: (agent: string) => void;
  onChanged: () => void;
  /** Reads "While you were away" again, after it was marked seen. */
  onAwayChanged: () => void;
}) {
  const [briefing, setBriefing] = useState<Briefing>();
  const [problem, setProblem] = useState("");
  const [loadProblem, setLoadProblem] = useState("");
  const [adding, setAdding] = useState(false);
  const [marked, setMarked] = useState(false);
  const [opened, setOpened] = useState<number[]>([]);
  // Items answered here, as the kernel answered: a finished one is no longer read, and its
  // line still says what became of it.
  const [answered, setAnswered] = useState<AgendaItem[]>([]);
  const kernel = away !== undefined;

  // Without the kernel's page, the engine's briefing of the last day (spec 3.3 §6).
  useEffect(() => {
    if (kernel) return;
    let live = true;
    const load = () =>
      client
        .briefing()
        .then((value) => {
          if (!live) return;
          setBriefing(value);
          setLoadProblem("");
        })
        .catch((error: unknown) => live && setLoadProblem(problemText(error)));
    void load();
    const timer = window.setInterval(load, 10_000);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, [client, kernel]);

  const known = agenda && [...agenda, ...answered.filter((item) => !agenda.some((read) => read.id === item.id))];
  const rows = kernel ? kernelRows(away, known, agents) : engineRows(briefing, agenda);

  async function answer(item: AgendaItem, yes: boolean) {
    setProblem("");
    try {
      const after = await client.answerAgenda(item.id, yes);
      if (after) setAnswered((all) => [...all.filter((each) => each.id !== after.id), after]);
      onChanged();
    } catch (error) {
      setProblem(problemText(error));
    }
  }

  async function markSeen(page: AwayPage, seam: WhileAwaySeam) {
    if (page.through === undefined) return;
    setProblem("");
    try {
      await seam.markSeen(page.through);
      setMarked(true);
      setOpened([]);
      onAwayChanged();
    } catch (error) {
      setProblem(problemText(error));
    }
  }

  const agentOf = (display: string) => agents.find((row) => row.agent.displayName === display)?.agent.name;
  const now = new Date();
  const windows = quietWindows(areas, status);
  const today = routinesToday.filter((item) => sameDay(item.at, now));
  const first = windows[0];
  const routines: Array<{ key: string; at: string; text: string; past: boolean }> = [
    ...today.map((item) => ({
      key: item.key,
      at: hhmm(item.at),
      text: item.agent ? `${item.agent} · ${item.title}` : item.title,
      past: item.at < now,
    })),
    ...(first
      ? [
          { key: "briefing", at: first.end, text: labels.morningBriefing, past: endToday(first, now) < now },
          { key: "quiet", at: first.start, text: labels.quietTimeStarts, past: false },
        ]
      : []),
  ].sort((a, b) => a.at.localeCompare(b.at));

  const since = away ? labels.since(dayAndTime(new Date(away.since))) : briefing?.sinceText;
  const quietValue = kernel
    ? windows.length === 0
      ? labels.notSet
      : away.quietTime.respected
        ? labels.quietRespected
        : labels.quietNotKept
    : first
      ? windowText(first)
      : labels.notSet;
  const quietLarge = kernel && windows.length > 0;
  const spent = away ? dollars(away.spentMicroUsd) : briefing?.spend ? dollars(briefing.spend.spentMicroUsd) : "—";
  const waiting = away ? away.waiting : (briefing?.waiting ?? status?.decisions ?? 0);
  const whileAway = seams.whileAway;
  const loaded = kernel || briefing !== undefined;

  return (
    <main className="today glow">
      <div className="today-main">
        {since && (
          <Pill kind="since" dot>
            {since}
          </Pill>
        )}
        <h1 className="display">
          {labels.awayLead} <span className="display-accent">{labels.awayAccent}</span>
        </h1>
        <p className="subhead">{labels.awaySubhead}</p>
        <div className="stats">
          <div className="stat">
            <span className="stat-label">{labels.spent}</span>
            <span className="stat-value">{spent}</span>
          </div>
          <div className="stat">
            <span className="stat-label">{labels.quietTime}</span>
            <span className={`stat-value${quietLarge ? "" : " stat-value-small"}`}>{quietValue}</span>
          </div>
          <div className="stat">
            <span className="stat-label">{labels.waitingForYou}</span>
            <span className="stat-value stat-accent">{waiting}</span>
          </div>
        </div>
        {away && whileAway && (
          <div className="briefing-tools">
            {marked && away.lines.length === 0 ? (
              <p className="muted" role="status">
                {labels.markedSeen}
              </p>
            ) : (
              <span />
            )}
            <button
              type="button"
              className="button button-secondary"
              disabled={away.through === undefined || away.lines.length === 0}
              onClick={() => void markSeen(away, whileAway)}
            >
              {labels.markSeen}
            </button>
          </div>
        )}
        {(problem || loadProblem) && (
          <p className="decision-problem" role="alert">
            {problem || loadProblem}
          </p>
        )}
        <ol className="briefing">
          {loaded && rows.length === 0 && (
            <li className="briefing-empty">
              {away ? labels.nothingSince(dayAndTime(new Date(away.since))) : labels.nothingHappened}
            </li>
          )}
          {rows.map((row) => {
            const time = (
              <time className="mono briefing-time" dateTime={row.at.toISOString()}>
                {hhmm(row.at)}
              </time>
            );
            if (row.kind === "away") {
              const open = opened.includes(row.line.seq);
              const item = row.item;
              const ask = item && agendaState(item) === "proposed";
              const question = row.line.kind === "question";
              return (
                <li key={row.key} id={`line-${row.line.seq}`} className="briefing-row">
                  {time}
                  <div className="briefing-body">
                    {row.said.pill && <Pill kind={row.said.pill}>{row.said.pillText}</Pill>}
                    <p className="briefing-text">{item ? visible(itemSentence(item)) : row.said.text}</p>
                    <p className="briefing-actions">
                      {ask && (
                        <>
                          <button type="button" className="link link-accent" onClick={() => void answer(item, true)}>
                            {question ? labels.yesRememberIt : labels.doIt}
                          </button>
                          <button type="button" className="link" onClick={() => void answer(item, false)}>
                            {question ? labels.no : labels.notNow}
                          </button>
                        </>
                      )}
                      {whileAway && (
                        <button
                          type="button"
                          className="link"
                          aria-expanded={open}
                          aria-controls={`record-${row.line.seq}`}
                          onClick={() =>
                            setOpened((all) => (open ? all.filter((seq) => seq !== row.line.seq) : [...all, row.line.seq]))
                          }
                        >
                          {labels.details}
                        </button>
                      )}
                    </p>
                    {open && whileAway && <RecordDetails id={`record-${row.line.seq}`} line={row.line} seam={whileAway} />}
                  </div>
                </li>
              );
            }
            if (row.kind === "line") {
              return (
                <li key={row.key} className="briefing-row">
                  {time}
                  <div className="briefing-body">
                    {row.line.pill && <Pill kind={pillKind[row.line.pill]}>{row.line.pillText}</Pill>}
                    <p className="briefing-text">{row.line.text}</p>
                    <p className="briefing-actions">
                      {agentOf(row.line.agent) && (
                        <button type="button" className="link" onClick={() => onOpenAgent(agentOf(row.line.agent)!)}>
                          {row.line.pill === "waitingForYou" ? labels.open : labels.details}
                        </button>
                      )}
                    </p>
                  </div>
                </li>
              );
            }
            const question = isQuestion(row.item);
            return (
              <li key={row.key} className="briefing-row">
                {time}
                <div className="briefing-body">
                  {question ? <Pill kind="question">{words.QUESTION}</Pill> : <Pill kind="suggestion">{words.SUGGESTION}</Pill>}
                  <p className="briefing-text">{visible(itemSentence(row.item))}</p>
                  <p className="briefing-actions">
                    <button type="button" className="link link-accent" onClick={() => void answer(row.item, true)}>
                      {question ? labels.yesRememberIt : labels.doIt}
                    </button>
                    <button type="button" className="link" onClick={() => void answer(row.item, false)}>
                      {question ? labels.no : labels.notNow}
                    </button>
                  </p>
                </div>
              </li>
            );
          })}
        </ol>
        {away?.more && <p className="muted briefing-more">{labels.moreToRead}</p>}
      </div>
      <aside className="today-side">
        <section className="card routines" aria-labelledby="routines-heading">
          <h2 id="routines-heading" className="card-heading">
            {labels.routinesToday}
          </h2>
          <ul className="routine-list">
            {routines.map((routine) => (
              <li key={routine.key} className={routine.past ? "past" : undefined}>
                <span className="mono">{routine.at}</span>
                <span>{routine.text}</span>
              </li>
            ))}
          </ul>
          {canAddRoutine && agents.length > 0 && !adding && (
            <button type="button" className="button button-secondary" onClick={() => setAdding(true)}>
              {labels.addRoutine}
            </button>
          )}
          {adding && (
            <AddRoutine
              agents={agents}
              onCancel={() => setAdding(false)}
              onAdd={async (target, text, at) => {
                await client.addRoutine(target, text, { anyOf: [{ cron: { expr: dailyCron(at), missed: "skip" } }] });
                setAdding(false);
                onChanged();
              }}
            />
          )}
        </section>
      </aside>
    </main>
  );
}

/**
 * The kernel's lines, each with its words; a suggestion or question with the agenda item it
 * made, which says what it is about and is answered here (spec 3.3 §5.1); and the open ones
 * from before the person last looked, which still wait for an answer. The agenda holds every
 * item still open or under way, so a suggestion or question with none of them is finished.
 */
function kernelRows(away: AwayPage, agenda: AgendaItem[] | undefined, agents: AgentRow[]): Row[] {
  const used = new Set<string>();
  const rows: Row[] = away.lines.map((line) => {
    const at = new Date(line.at);
    let item: AgendaItem | undefined;
    if (line.kind === "suggestion" || line.kind === "question") {
      item = (agenda ?? []).find(
        (candidate) =>
          !used.has(candidate.id) &&
          isQuestion(candidate) === (line.kind === "question") &&
          Math.abs(new Date(candidate.createdAt).getTime() - at.getTime()) <= SAME_EVENT_MS,
      );
      if (item) used.add(item.id);
    }
    let said = lineWords(line, agents);
    if (agenda && !item && (line.kind === "suggestion" || line.kind === "question")) {
      said = {
        ...said,
        text: line.kind === "question" ? labels.noticedSomething : labels.suggestedSomething(agentName(line.agent, agents) ?? words.YOUR_AGENTS),
      };
    }
    return { kind: "away", key: `k${line.seq}`, at, failed: line.kind === "failed", line, said, item };
  });
  for (const item of agenda ?? []) {
    if (used.has(item.id) || agendaState(item) !== "proposed") continue;
    rows.push({ kind: "ask", key: `a${item.id}`, at: new Date(item.createdAt), failed: false, item });
  }
  return inOrder(rows);
}

/**
 * The engine's briefing lines; where the kernel keeps an agenda, its open suggestions and
 * questions take the place of their ledger lines, as they are the same events.
 */
function engineRows(briefing: Briefing | undefined, agenda: AgendaItem[] | undefined): Row[] {
  const asks = (agenda ?? []).filter((item) => agendaState(item) === "proposed");
  return [
    ...(briefing?.lines ?? [])
      .filter((line) => !(agenda && (line.pill === "suggestion" || line.pill === "question")))
      .map((line): Row => ({ kind: "line", key: `l${line.seq}`, at: new Date(line.at), failed: false, line })),
    ...asks.map((item): Row => ({ kind: "ask", key: `a${item.id}`, at: new Date(item.createdAt), failed: false, item })),
  ].sort((a, b) => a.at.getTime() - b.at.getTime());
}

/**
 * One line's record (`GET /kernel/away/lines/{seq}`), read with the page's own sign-in and told
 * in plain words, with its entry on the record (spec 3.3 §5.1, G3). The route's raw row is no
 * page of its own: a link to it would leave the home, without the token behind `--auth-token`.
 */
function RecordDetails({ id, line, seam }: { id: string; line: AwayLine; seam: WhileAwaySeam }) {
  const [record, setRecord] = useState<AwayRecord>();
  const [problem, setProblem] = useState("");
  useEffect(() => {
    let live = true;
    seam
      .record(line.seq)
      .then((value) => live && setRecord(value))
      .catch((error: unknown) => live && setProblem(problemText(error)));
    return () => {
      live = false;
    };
  }, [seam, line.seq]);
  const entry = record?.entry;
  const spend = entry?.meta?.micro_usd ?? entry?.meta?.cost_micro_usd;
  const when = entry ? new Date(entry.ts) : undefined;
  return (
    <div id={id} className="record-details">
      {problem && (
        <p className="decision-problem" role="alert">
          {problem}
        </p>
      )}
      {when && entry && (
        <ul>
          <li>{labels.onRecord(line.seq)}</li>
          <li>{labels.recordedAt(`${dayMonth(when)}, ${hhmm(when)}`)}</li>
          {entry.partition !== "default" && <li>{labels.inAreaShort(entry.partition)}</li>}
          {typeof spend === "number" && <li>{labels.recordSpent(dollars(spend))}</li>}
          {typeof entry.durationMs === "number" && <li>{labels.recordTook(seconds(entry.durationMs))}</li>}
        </ul>
      )}
    </div>
  );
}

/** A duration in plain words: "3 seconds", "2 minutes". */
function seconds(ms: number): string {
  const s = Math.max(1, Math.round(ms / 1000));
  if (s < 120) return s === 1 ? "1 second" : `${s} seconds`;
  return `${Math.round(s / 60)} minutes`;
}

/** "Add a routine": what should happen, every day at a time, by which agent. */
function AddRoutine({
  agents,
  onCancel,
  onAdd,
}: {
  agents: AgentRow[];
  onCancel: () => void;
  onAdd: (target: string, text: string, at: string) => Promise<void>;
}) {
  const [text, setText] = useState("");
  const [at, setAt] = useState("09:00");
  const [target, setTarget] = useState(agents[0]?.agent.name ?? "");
  const [problem, setProblem] = useState("");
  const ids = { what: useId(), when: useId(), who: useId() };
  return (
    <form
      className="routine-form"
      onSubmit={async (event) => {
        event.preventDefault();
        if (!text.trim() || !target) return;
        try {
          await onAdd(target, text.trim(), at);
        } catch (error) {
          setProblem(problemText(error));
        }
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") onCancel();
      }}
    >
      <label htmlFor={ids.what}>{labels.routineWhat}</label>
      <input id={ids.what} value={text} onChange={(event) => setText(event.target.value)} autoFocus />
      <label htmlFor={ids.when}>{labels.routineWhen}</label>
      <input id={ids.when} type="time" value={at} onChange={(event) => setAt(event.target.value)} />
      {agents.length > 1 && (
        <>
          <label htmlFor={ids.who}>{labels.routineWho}</label>
          <select id={ids.who} value={target} onChange={(event) => setTarget(event.target.value)}>
            {agents.map((row) => (
              <option key={row.agent.name} value={row.agent.name}>
                {row.agent.displayName}
              </option>
            ))}
          </select>
        </>
      )}
      {problem && (
        <p className="decision-problem" role="alert">
          {problem}
        </p>
      )}
      <div className="decision-actions">
        <button type="submit" className="button button-primary">
          {labels.routineSave}
        </button>
        <button type="button" className="button button-quiet" onClick={onCancel}>
          {labels.cancel}
        </button>
      </div>
    </form>
  );
}
