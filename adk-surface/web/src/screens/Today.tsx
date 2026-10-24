import { useEffect, useId, useState } from "react";
import type { AgentRow, Briefing, BriefingLine, Pill as PillKindApi, StatusLine } from "../generated/api";
import type { HomeClient } from "../client";
import { problemText } from "../client";
import { Pill } from "../components/Pill";
import type { PillKind } from "../components/Pill";
import { dollars, hhmm, sameDay } from "../format";
import type { AgendaItem } from "../kernel";
import { agendaState, dailyCron, isQuestion } from "../kernel";
import { labels, words } from "../labels";
import type { Planned } from "../plan";
import type { Seams, WhileAway } from "../seams";

const pillKind: Record<PillKindApi, PillKind> = {
  done: "done",
  failed: "failed",
  waitingForYou: "waiting",
  youDecided: "decided",
  suggestion: "suggestion",
  question: "question",
};

/** A line of the timeline: one from the ledger, or an open suggestion or question. */
type Row =
  | { kind: "line"; key: string; at: Date; line: BriefingLine }
  | { kind: "ask"; key: string; at: Date; item: AgendaItem };

/**
 * Today — "While you were away" (spec 3.3 §5.1): what was spent, whether quiet time was
 * kept, how many decisions wait, then one line per event since the person left, each with a
 * status pill and what can be done about it, and today's routines.
 */
export function Today({
  client,
  agents,
  status,
  agenda,
  routinesToday,
  canAddRoutine,
  seams,
  onOpenAgent,
  onChanged,
}: {
  client: HomeClient;
  agents: AgentRow[];
  status?: StatusLine;
  agenda?: AgendaItem[];
  /** Today's routines, past ones included. */
  routinesToday: Planned[];
  /** The kernel serves its process table, where a routine is a sleeping process. */
  canAddRoutine: boolean;
  seams: Seams;
  onOpenAgent: (agent: string) => void;
  onChanged: () => void;
}) {
  const [briefing, setBriefing] = useState<Briefing>();
  const [away, setAway] = useState<WhileAway>();
  const [problem, setProblem] = useState("");
  const [loadProblem, setLoadProblem] = useState("");
  const [adding, setAdding] = useState(false);

  useEffect(() => {
    let live = true;
    const load = () =>
      client
        .briefing()
        .then((value) => {
          if (!live) return;
          setBriefing(value);
          setLoadProblem("");
          if (seams.whileAway) void seams.whileAway.read(value.since).then((read) => live && setAway(read));
        })
        .catch((error: unknown) => live && setLoadProblem(problemText(error)));
    void load();
    const timer = window.setInterval(load, 10_000);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, [client, seams.whileAway]);

  // Open suggestions and questions are answered here (spec 3.3 §5.1); their ledger lines are
  // the same events, so the agenda's own lines take their place.
  const asks = (agenda ?? []).filter((item) => agendaState(item) === "proposed");
  const rows: Row[] = [
    ...(briefing?.lines ?? [])
      .filter((line) => !(agenda && (line.pill === "suggestion" || line.pill === "question")))
      .map((line): Row => ({ kind: "line", key: `l${line.seq}`, at: new Date(line.at), line })),
    ...asks.map((item): Row => ({ kind: "ask", key: `a${item.id}`, at: new Date(item.createdAt), item })),
  ].sort((a, b) => a.at.getTime() - b.at.getTime());

  async function answer(item: AgendaItem, yes: boolean) {
    setProblem("");
    try {
      await client.answerAgenda(item.id, yes);
      onChanged();
    } catch (error) {
      setProblem(problemText(error));
    }
  }

  const agentOf = (display: string) => agents.find((row) => row.agent.displayName === display)?.agent.name;
  const now = new Date();
  const today = routinesToday.filter((item) => sameDay(item.at, now));
  const quiet = status?.quiet;
  const routines: Array<{ key: string; at: string; text: string; past: boolean }> = [
    ...today.map((item) => ({
      key: item.key,
      at: hhmm(item.at),
      text: item.agent ? `${item.agent} · ${item.title}` : item.title,
      past: item.at < now,
    })),
    ...(quiet ? [{ key: "quiet", at: quiet.start, text: labels.quietTimeStarts, past: false }] : []),
  ].sort((a, b) => a.at.localeCompare(b.at));

  return (
    <main className="today glow">
      <div className="today-main">
        {briefing && (
          <Pill kind="since" dot>
            {briefing.sinceText}
          </Pill>
        )}
        <h1 className="display">
          {labels.awayLead} <span className="display-accent">{labels.awayAccent}</span>
        </h1>
        <p className="subhead">{labels.awaySubhead}</p>
        <div className="stats">
          <div className="stat">
            <span className="stat-label">{labels.spent}</span>
            <span className="stat-value">{briefing?.spend ? dollars(briefing.spend.spentMicroUsd) : "—"}</span>
          </div>
          <div className="stat">
            <span className="stat-label">{labels.quietTime}</span>
            <span className={`stat-value${away ? "" : " stat-value-small"}`}>
              {away
                ? away.quietRespected
                  ? labels.quietRespected
                  : labels.quietNotKept
                : quiet
                  ? `${quiet.start}–${quiet.end}`
                  : labels.notSet}
            </span>
          </div>
          <div className="stat">
            <span className="stat-label">{labels.waitingForYou}</span>
            <span className="stat-value stat-accent">{briefing?.waiting ?? status?.decisions ?? 0}</span>
          </div>
        </div>
        {(problem || loadProblem) && (
          <p className="decision-problem" role="alert">
            {problem || loadProblem}
          </p>
        )}
        <ol className="briefing">
          {briefing && rows.length === 0 && <li className="briefing-empty">{labels.nothingHappened}</li>}
          {rows.map((row) =>
            row.kind === "line" ? (
              <li key={row.key} className="briefing-row">
                <time className="mono briefing-time" dateTime={row.line.at}>
                  {row.line.atText}
                </time>
                <div className="briefing-body">
                  {row.line.pill && <Pill kind={pillKind[row.line.pill]}>{row.line.pillText}</Pill>}
                  <p className="briefing-text">{row.line.text}</p>
                  <p className="briefing-actions">
                    {away?.records[row.line.seq] ? (
                      <a className="link" href={away.records[row.line.seq]}>
                        {labels.details}
                      </a>
                    ) : (
                      agentOf(row.line.agent) && (
                        <button type="button" className="link" onClick={() => onOpenAgent(agentOf(row.line.agent)!)}>
                          {row.line.pill === "waitingForYou" ? labels.open : labels.details}
                        </button>
                      )
                    )}
                  </p>
                </div>
              </li>
            ) : (
              <li key={row.key} className="briefing-row">
                <time className="mono briefing-time" dateTime={row.item.createdAt}>
                  {hhmm(row.at)}
                </time>
                <div className="briefing-body">
                  {isQuestion(row.item) ? (
                    <Pill kind="question">{words.QUESTION}</Pill>
                  ) : (
                    <Pill kind="suggestion">{words.SUGGESTION}</Pill>
                  )}
                  <p className="briefing-text">
                    {isQuestion(row.item) ? `${row.item.title} ${words.IS_THAT_RIGHT}` : `${row.item.title} ${labels.nothingHappensUntilYouSay}`}
                  </p>
                  <p className="briefing-actions">
                    <button type="button" className="link link-accent" onClick={() => void answer(row.item, true)}>
                      {isQuestion(row.item) ? labels.yesRememberIt : labels.doIt}
                    </button>
                    <button type="button" className="link" onClick={() => void answer(row.item, false)}>
                      {isQuestion(row.item) ? labels.no : labels.notNow}
                    </button>
                  </p>
                </div>
              </li>
            ),
          )}
        </ol>
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
