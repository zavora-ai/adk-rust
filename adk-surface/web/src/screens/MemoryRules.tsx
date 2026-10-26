import { useEffect, useId, useMemo, useState } from "react";
import type { ReactNode } from "react";
import type { AgentRow, Briefing, StatusLine } from "../generated/api";
import type { HomeClient } from "../client";
import { problemText } from "../client";
import { AreaRoster, areaAgents } from "../components/AreaRoster";
import { Icon } from "../components/Icon";
import { Pill } from "../components/Pill";
import { dayMonth, dollars, hhmm, parseDollars, sameDay } from "../format";
import type { AreaSettings, ChannelWaiting, KernelRecordLine, QuietWindow, StandingRule } from "../kernel";
import { accountName, allows, channelName } from "../kernel";
import { labels, modes, words } from "../labels";
import { undoText } from "../memory";
import { phoneLabels } from "../phone/labels";
import { RECORD_READ, recentLines } from "../record";
import { dateValue, earliestExtension, endOfDay, ended, ruleSentence, suggestedExtension } from "../rules";
import type { AgentSpend, Device, MemoryFact, PairingIssued, RecordCheck, RecordLine, Seams, TidyUp } from "../seams";
import { agentOf, onThisDevice, typedWindow } from "../away";
import { areaName, kernelMode, limitValue, modeOf, modeValue, quietValue, windowText } from "../settings";

const provenance = {
  youSaidThis: { text: labels.youSaidThis, kind: "decided" },
  youConfirmedThis: { text: labels.youConfirmedThis, kind: "decided" },
  aGuess: { text: labels.aGuess, kind: "neutral" },
} as const;

/**
 * Memory & rules — "Agent Memory and Rules" (spec 3.3 §5.1): what your agents remember, what
 * they can do, and what they did, as three bordered lists. Controls whose routes the server
 * serves act; the rest show what exists, read-only, until their seam is given (§10 K3, K5).
 */
export function MemoryRules({
  client,
  status,
  areas,
  agents = [],
  seams,
  onChanged,
}: {
  client: HomeClient;
  status?: StatusLine;
  /** Each area's settings in force (spec 3.3 §10 K3), when the kernel serves them. */
  areas?: AreaSettings[];
  /** The roster, to name agents and draw their avatars as Threads does. */
  agents?: AgentRow[];
  seams: Seams;
  onChanged: () => void;
}) {
  return (
    <main className="memory glow">
      <h1 className="display display-memory">
        {labels.memoryLead} <span className="display-accent">{labels.memoryAccent}</span>
      </h1>
      <p className="subhead subhead-memory">{labels.memorySubhead}</p>
      <div className="memory-grid">
        <Remember seams={seams} agents={agents} />
        <CanDo client={client} status={status} areas={areas} agents={agents} seams={seams} onChanged={onChanged} />
        <Did client={client} agents={agents} seams={seams} />
      </div>
    </main>
  );
}

function Remember({ seams, agents }: { seams: Seams; agents: AgentRow[] }) {
  const [facts, setFacts] = useState<MemoryFact[]>([]);
  // The agents keep long-term memory: the kernel's memory route has a book to read.
  const [kept, setKept] = useState(true);
  const [tidyUp, setTidyUp] = useState<TidyUp>();
  const [problem, setProblem] = useState("");
  const [note, setNote] = useState("");
  const [fixing, setFixing] = useState<string>();
  const [fixText, setFixText] = useState("");
  const fixField = useId();
  const memory = seams.memory;

  const load = () =>
    memory
      ?.facts()
      .then((read) => {
        setKept(read !== undefined);
        setFacts(read?.facts ?? []);
        setTidyUp(read?.tidyUp);
      })
      .catch((error: unknown) => setProblem(problemText(error)));
  useEffect(() => {
    void load();
  }, [memory]);

  /** Runs one correction, reads the memory again, and says what became of it (spec 3.3 §8 "Name the outcome"). */
  async function act<T>(action: () => Promise<T>, said?: (result: T) => string): Promise<boolean> {
    setProblem("");
    setNote("");
    try {
      const result = await action();
      await load();
      if (said) setNote(said(result));
      return true;
    } catch (error) {
      setProblem(problemText(error));
      return false;
    }
  }

  return (
    <section className="card memory-card" aria-labelledby="remember-heading">
      <h2 id="remember-heading" className="card-heading">
        {labels.whatTheyRemember}
      </h2>
      {tidyUp && memory && (
        <div className="tidy-up">
          <p>{tidyUp.text}</p>
          <button type="button" className="button button-secondary" onClick={() => void act(() => memory.undo(tidyUp.id), undoText)}>
            {labels.undo}
          </button>
        </div>
      )}
      {problem && (
        <p className="decision-problem" role="alert">
          {problem}
        </p>
      )}
      {note && (
        <p className="change-note" role="status">
          {note}
        </p>
      )}
      {facts.length === 0 ? (
        <p className="card-empty">{kept ? labels.rememberEmpty : labels.memoryOff}</p>
      ) : (
        <ul className="bordered">
          {facts.map((fact) => (
            <li key={fact.id} className="fact">
              <p className="fact-text">{fact.text}</p>
              <p className="fact-meta">
                <Pill kind={provenance[fact.provenance].kind}>{provenance[fact.provenance].text}</Pill>
                {fact.provenance === "aGuess" && fact.noticedBy && (
                  <span>{labels.noticedThis(agentOf(fact.noticedBy, agents) ?? fact.noticedBy)}</span>
                )}
              </p>
              {/* Every fact can be fixed or forgotten; only a guess can be confirmed. */}
              {memory && fixing !== fact.id && (
                <p className="row-actions">
                  {fact.provenance === "aGuess" && (
                    <button type="button" className="link link-accent" onClick={() => void act(() => memory.confirm(fact.id))}>
                      {labels.thatsRight}
                    </button>
                  )}
                  <button
                    type="button"
                    className="link"
                    onClick={() => {
                      setFixing(fact.id);
                      setFixText(fact.text);
                    }}
                  >
                    {labels.fixIt}
                  </button>
                  <button
                    type="button"
                    className="link"
                    onClick={() => void act(() => memory.forget(fact.id), () => labels.forgotten)}
                  >
                    {labels.forgetIt}
                  </button>
                </p>
              )}
              {fixing === fact.id && memory && (
                <form
                  className="inline-form"
                  onSubmit={(event) => {
                    event.preventDefault();
                    const text = fixText.trim();
                    if (!text) return;
                    void act(() => memory.fix(fact.id, text), () => labels.fixed).then((done) => {
                      if (done) setFixing(undefined);
                    });
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Escape") setFixing(undefined);
                  }}
                >
                  <label htmlFor={fixField} className="visually-hidden">
                    {labels.fixedText}
                  </label>
                  <input id={fixField} value={fixText} onChange={(event) => setFixText(event.target.value)} autoFocus />
                  <button type="submit" className="button button-secondary" disabled={!fixText.trim()}>
                    {labels.fixIt}
                  </button>
                </form>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function CanDo({
  client,
  status,
  areas,
  agents,
  seams,
  onChanged,
}: {
  client: HomeClient;
  status?: StatusLine;
  areas?: AreaSettings[];
  agents: AgentRow[];
  seams: Seams;
  onChanged: () => void;
}) {
  const [rules, setRules] = useState<StandingRule[]>();
  const [problem, setProblem] = useState("");
  const [chosen, setChosen] = useState<string>();
  // What each change was and became, by area and setting, as the kernel recorded it.
  const [notes, setNotes] = useState<Record<string, string>>({});
  // The rule whose end is being moved, and what each extension changed.
  const [extending, setExtending] = useState<string>();
  const [extended, setExtended] = useState<Record<string, string>>({});
  const [spending, setSpending] = useState<AgentSpend[]>();
  const limits = seams.limits;
  const roster = seams.roster;
  const kernelAreas = limits && areas && areas.length > 0 ? areas : undefined;
  const area = kernelAreas?.find((candidate) => candidate.partition === chosen) ?? kernelAreas?.[0];

  const load = () => client.rules().then(setRules).catch((error: unknown) => setProblem(problemText(error)));
  useEffect(() => {
    void load();
  }, [client]);
  // Spending is read again when an area's "spent today" moves, so an agent's spend and its
  // area's total come from the same moment of the kernel's daily totals. The read lists every
  // process the kernel has run, so the areas' poll alone, which brings the same totals in a new
  // list, never repeats it.
  const spentKey = areas?.map((entry) => `${entry.partition}:${entry.spentToday}`).join(",") ?? "";
  useEffect(() => {
    let live = true;
    void roster
      ?.spending()
      .then((value) => live && setSpending(value))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [roster, spentKey]);

  /** "Extend": moves a rule's end later, then shows what it was and is (spec 3.3 §8.1). */
  async function extend(rule: StandingRule, until: Date): Promise<boolean> {
    if (!client.extendRule) return false;
    setProblem("");
    try {
      const after = await client.extendRule(rule.id, until.toISOString());
      const was = rule.expiresAt ? labels.untilWords(dayMonth(new Date(rule.expiresAt))) : labels.noEndDate;
      const now = after.expiresAt ? labels.untilWords(dayMonth(new Date(after.expiresAt))) : labels.noEndDate;
      setExtended((all) => ({ ...all, [rule.id]: labels.wasNow(was, now) }));
      await load();
      return true;
    } catch (error) {
      setProblem(problemText(error));
      return false;
    }
  }

  async function act(action: () => Promise<void>) {
    setProblem("");
    try {
      await action();
      await load();
      onChanged();
    } catch (error) {
      setProblem(problemText(error));
    }
  }

  /**
   * Changes one setting of the area shown (spec 3.3 §10 K3): reads the area just before, so
   * the old value shown is the one the kernel records as `from`, then shows it beside the new.
   */
  async function change(
    setting: "quiet" | "limit" | "mode",
    apply: (partition: string) => Promise<AreaSettings>,
    value: (area: AreaSettings) => string,
  ): Promise<boolean> {
    if (!area || !limits) return false;
    setProblem("");
    try {
      const before = await limits.area(area.partition);
      const after = await apply(area.partition);
      setNotes((all) => ({ ...all, [`${area.partition}:${setting}`]: labels.wasNow(value(before), value(after)) }));
      onChanged();
      return true;
    } catch (error) {
      setProblem(problemText(error));
      return false;
    }
  }

  const allowed = (rules ?? []).filter(allows);
  // What was spent in an area the kernel does not list, such as its own upkeep in `default`
  // beside configured areas: no area's roster shows it, so it has a list of its own.
  const listed = new Set(kernelAreas?.map((entry) => entry.partition) ?? []);
  const outside = kernelAreas
    ? (spending ?? []).filter((entry) => !listed.has(entry.area)).map((entry) => ({ ...entry, area: "" }))
    : [];
  const current = area?.mode ? modeOf(area.mode) : status?.mode?.mode;
  // Quiet time on this device's clock: the kernel keeps a window without an offset in the
  // server's time zone (spec 3.3 §5.1).
  const host = status?.hostUtcOffsetMinutes ?? undefined;
  const here = (window: QuietWindow) => windowText(onThisDevice(window, host));
  const note = (setting: string) => (area ? notes[`${area.partition}:${setting}`] : undefined);

  return (
    <section className="card memory-card" aria-labelledby="can-do-heading">
      <h2 id="can-do-heading" className="card-heading">
        {labels.whatTheyCanDo}
      </h2>
      {kernelAreas && kernelAreas.length > 1 && (
        <div className="segmented segmented-areas" role="radiogroup" aria-label={labels.areas}>
          {kernelAreas.map((candidate) => (
            <button
              key={candidate.partition}
              type="button"
              role="radio"
              aria-checked={candidate.partition === area?.partition}
              className="segment"
              onClick={() => setChosen(candidate.partition)}
            >
              {areaName(candidate.partition) ?? words.YOUR_AGENTS}
            </button>
          ))}
        </div>
      )}
      {current && (
        <>
          <p className="card-label" id="mode-label">
            {area && areaName(area.partition) ? labels.inArea(areaName(area.partition)!) : labels.yourAgentsMode}
          </p>
          {area && limits ? (
            <div className="segmented" role="radiogroup" aria-labelledby="mode-label">
              {modes.map(({ mode, label }) => (
                <button
                  key={mode}
                  type="button"
                  role="radio"
                  aria-checked={mode === current}
                  className="segment"
                  onClick={() => {
                    if (mode !== current) void change("mode", (partition) => limits.setMode(partition, kernelMode(mode)), modeValue);
                  }}
                >
                  {label}
                </button>
              ))}
            </div>
          ) : (
            <ul className="segmented" aria-labelledby="mode-label">
              {modes.map(({ mode, label }) => (
                <li key={mode} className="segment" aria-current={mode === current ? "true" : undefined}>
                  {label}
                </li>
              ))}
            </ul>
          )}
          {note("mode") && (
            <p className="change-note" role="status">
              {note("mode")}
            </p>
          )}
        </>
      )}
      {problem && (
        <p className="decision-problem" role="alert">
          {problem}
        </p>
      )}
      {rules !== undefined && (
        <>
          <p className="card-label">{labels.allowedWithoutAsking}</p>
          {allowed.length === 0 ? (
            <p className="card-empty">{labels.allowedEmpty}</p>
          ) : (
            <ul className="bordered">
              {allowed.map((rule) => (
                <RuleRow
                  key={rule.id}
                  rule={rule}
                  text={ruleSentence(rule, agents)}
                  note={extended[rule.id]}
                  editing={extending === rule.id}
                  canExtend={!!client.extendRule}
                  onExtend={() => setExtending(rule.id)}
                  onCancel={() => setExtending(undefined)}
                  onSave={async (until) => {
                    if (await extend(rule, until)) setExtending(undefined);
                  }}
                  onStop={() => void act(() => client.revokeRule(rule.id))}
                />
              ))}
            </ul>
          )}
        </>
      )}
      <p className="always-asks">{labels.alwaysAsks}</p>
      {area && limits ? (
        <ul className="bordered settings">
          <SettingRow
            label={labels.quietTime}
            value={area.quietTime.length === 0 ? labels.off : area.quietTime.map(here).join(", ")}
            sub={[
              area.alsoQuiet?.length ? labels.workspaceAlsoQuiet(area.alsoQuiet.map(here).join(", ")) : undefined,
              !note("quiet") && area.changed?.quietTime ? labels.changedAt(changedWhen(area.changed.quietTime.at)) : undefined,
            ]}
            note={note("quiet")}
            editor={(close) => (
              <QuietEditor
                window={area.quietTime[0] && onThisDevice(area.quietTime[0], host)}
                others={area.quietTime.length > 1 ? area.quietTime.map(here).join(", ") : undefined}
                onCancel={close}
                onSave={async (window) => {
                  const value = (read: AreaSettings) => quietValue(read, host);
                  if (await change("quiet", (partition) => limits.setQuietTime(partition, window), value)) close();
                }}
              />
            )}
          />
          <SettingRow
            label={labels.dailySpendingLimit}
            value={limitValue(area)}
            sub={[
              labels.spentToday(dollars(area.spentToday)),
              !note("limit") && area.changed?.dailySpendingLimit
                ? labels.changedAt(changedWhen(area.changed.dailySpendingLimit.at))
                : undefined,
            ]}
            note={note("limit")}
            editor={(close) => (
              <LimitEditor
                microUsd={area.dailySpendingLimit}
                onCancel={close}
                onSave={async (microUsd) => {
                  if (await change("limit", (partition) => limits.setSpendingLimit(partition, microUsd), limitValue)) close();
                }}
              />
            )}
          />
        </ul>
      ) : (
        <ul className="bordered settings">
          <SettingRow
            label={labels.quietTime}
            value={status?.quiet ? here({ start: status.quiet.start, end: status.quiet.end }) : labels.notSet}
          />
          <SettingRow
            label={labels.dailySpendingLimit}
            value={
              status?.spend?.limitMicroUsd !== undefined && status?.spend?.limitMicroUsd !== null
                ? dollars(status.spend.limitMicroUsd)
                : labels.notSet
            }
          />
        </ul>
      )}
      {spending && (
        <AreaRoster
          agents={areaAgents(area?.partition ?? "default", spending, agents, !kernelAreas || kernelAreas.length < 2)}
          label={area && areaName(area.partition) ? labels.agentsTodayIn(areaName(area.partition)!) : labels.agentsToday}
        />
      )}
      {outside.some((entry) => entry.spentToday > 0) && (
        <AreaRoster agents={areaAgents("", outside, agents, false)} label={labels.outsideAreas} />
      )}
    </section>
  );
}

/**
 * One rule allowed without asking: what it lets agents do, its end date, **Extend** for a rule
 * with one, and **Stop allowing** (spec 3.3 §8.1). An ended rule says so and can be extended
 * again; Extend opens a date in place, and Esc closes it without changing anything.
 */
function RuleRow({
  rule,
  text,
  note,
  editing,
  canExtend,
  onExtend,
  onCancel,
  onSave,
  onStop,
}: {
  rule: StandingRule;
  text: string;
  note?: string;
  editing: boolean;
  canExtend: boolean;
  onExtend: () => void;
  onCancel: () => void;
  onSave: (until: Date) => Promise<void>;
  onStop: () => void;
}) {
  const textId = useId();
  const field = useId();
  const now = new Date();
  // Set again each time Extend opens, so it offers thirty days past the rule's end as it is then.
  const [value, setValue] = useState(() => dateValue(suggestedExtension(rule, now)));
  const min = dateValue(earliestExtension(rule, now));
  const until = endOfDay(value);
  const valid = until !== undefined && value >= min;
  const end = rule.expiresAt ? new Date(rule.expiresAt) : undefined;
  return (
    <li className="rule">
      <p className="rule-text" id={textId}>
        {text}
      </p>
      {editing ? (
        <form
          className="setting-form"
          aria-describedby={textId}
          onSubmit={(event) => {
            event.preventDefault();
            if (valid) void onSave(until);
          }}
          onKeyDown={(event) => {
            if (event.key === "Escape") onCancel();
          }}
        >
          <div className="setting-fields">
            <label htmlFor={field}>{labels.extendUntil}</label>
            <input
              id={field}
              type="date"
              min={min}
              value={value}
              aria-invalid={!valid}
              onChange={(event) => setValue(event.target.value)}
              autoFocus
            />
          </div>
          <p className="setting-sub">{labels.extendHint}</p>
          <div className="setting-buttons">
            <button type="submit" className="button button-secondary" disabled={!valid}>
              {labels.extend}
            </button>
            <button type="button" className="button button-quiet" onClick={onCancel}>
              {labels.cancel}
            </button>
          </div>
        </form>
      ) : (
        <p className="row-actions">
          <span className="muted">
            {end ? (ended(rule, now) ? labels.endedOn(dayMonth(end)) : labels.until(dayMonth(end))) : labels.noEndDate}
          </span>
          {end && canExtend && (
            <button
              type="button"
              className="link link-accent"
              aria-describedby={textId}
              onClick={() => {
                setValue(dateValue(suggestedExtension(rule, new Date())));
                onExtend();
              }}
            >
              {labels.extend}
            </button>
          )}
          <button type="button" className="link" aria-describedby={textId} onClick={onStop}>
            {labels.stopAllowing}
          </button>
        </p>
      )}
      {note && (
        <p className="change-note" role="status">
          {note}
        </p>
      )}
    </li>
  );
}

/** "at 18:02" today, else "on 23 October". */
function changedWhen(at: string): string {
  const when = new Date(at);
  return sameDay(when, new Date()) ? labels.atTime(hhmm(when)) : `on ${dayMonth(when)}`;
}

/**
 * One setting: its value, what goes with it, the change just made, and **Change**, which opens
 * its editor in place; Esc closes it without changing anything.
 */
function SettingRow({
  label,
  value,
  sub = [],
  note,
  editor,
}: {
  label: string;
  value: string;
  sub?: Array<string | undefined>;
  note?: string;
  editor?: (close: () => void) => ReactNode;
}) {
  const [editing, setEditing] = useState(false);
  const heading = useId();
  return (
    <li className="setting" aria-labelledby={heading}>
      <div className="setting-main">
        <p className="setting-label" id={heading}>
          {label}
        </p>
        {editing && editor ? (
          editor(() => setEditing(false))
        ) : (
          <>
            <p className="setting-value">{value}</p>
            {sub
              .filter((line): line is string => !!line)
              .map((line) => (
                <p key={line} className="setting-sub">
                  {line}
                </p>
              ))}
          </>
        )}
        {note && (
          <p className="change-note" role="status">
            {note}
          </p>
        )}
      </div>
      {editor && !editing && (
        <button type="button" className="link link-accent" aria-describedby={heading} onClick={() => setEditing(true)}>
          {labels.change}
        </button>
      )}
    </li>
  );
}

/**
 * Quiet time from one time to another, or off, on this device's clock: the window is saved with
 * this device's offset, so the kernel keeps it at the hours the person typed.
 */
function QuietEditor({
  window,
  others,
  onSave,
  onCancel,
}: {
  window?: QuietWindow;
  /** Every window in force, when there are several: the kernel keeps one in their place. */
  others?: string;
  onSave: (window: QuietWindow | null) => Promise<void>;
  onCancel: () => void;
}) {
  const [start, setStart] = useState(window?.start ?? "22:00");
  const [end, setEnd] = useState(window?.end ?? "07:00");
  const ids = { start: useId(), end: useId() };
  const same = start === end;
  return (
    <form
      className="setting-form"
      onSubmit={(event) => {
        event.preventDefault();
        if (!same) void onSave(typedWindow(start, end));
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") onCancel();
      }}
    >
      {others && <p className="setting-sub">{labels.replacesWindows(others)}</p>}
      <div className="setting-fields">
        <label htmlFor={ids.start}>{labels.from}</label>
        <input id={ids.start} type="time" value={start} onChange={(event) => setStart(event.target.value)} autoFocus />
        <label htmlFor={ids.end}>{labels.to}</label>
        <input id={ids.end} type="time" value={end} onChange={(event) => setEnd(event.target.value)} />
      </div>
      <div className="setting-buttons">
        <button type="submit" className="button button-secondary" disabled={same}>
          {labels.save}
        </button>
        {window && (
          <button type="button" className="button button-quiet" onClick={() => void onSave(null)}>
            {labels.turnOff}
          </button>
        )}
        <button type="button" className="button button-quiet" onClick={onCancel}>
          {labels.cancel}
        </button>
      </div>
    </form>
  );
}

/** The daily spending limit in dollars, or none. */
function LimitEditor({
  microUsd,
  onSave,
  onCancel,
}: {
  microUsd?: number;
  onSave: (microUsd: number | null) => Promise<void>;
  onCancel: () => void;
}) {
  const [text, setText] = useState(microUsd === undefined ? "" : (microUsd / 1_000_000).toFixed(2));
  const [invalid, setInvalid] = useState(false);
  const field = useId();
  return (
    <form
      className="setting-form"
      onSubmit={(event) => {
        event.preventDefault();
        const microUsd = parseDollars(text);
        if (microUsd === undefined) {
          setInvalid(true);
          return;
        }
        setInvalid(false);
        void onSave(microUsd);
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") onCancel();
      }}
    >
      <div className="setting-fields">
        <label htmlFor={field}>{labels.limitInDollars}</label>
        <input
          id={field}
          inputMode="decimal"
          value={text}
          aria-invalid={invalid}
          onChange={(event) => setText(event.target.value)}
          autoFocus
        />
      </div>
      {invalid && (
        <p className="decision-problem" role="alert">
          {labels.notAMoney}
        </p>
      )}
      <div className="setting-buttons">
        <button type="submit" className="button button-secondary">
          {labels.save}
        </button>
        {microUsd !== undefined && (
          <button type="button" className="button button-quiet" onClick={() => void onSave(null)}>
            {labels.noLimit}
          </button>
        )}
        <button type="button" className="button button-quiet" onClick={onCancel}>
          {labels.cancel}
        </button>
      </div>
    </form>
  );
}

/** "just now" within a minute of the check, else "at 07:31". */
function checkedWhen(at: string): string {
  const when = new Date(at);
  return Date.now() - when.getTime() < 60_000 ? labels.justNow : labels.atTime(hhmm(when));
}

function Did({ client, agents, seams }: { client: HomeClient; agents: AgentRow[]; seams: Seams }) {
  const [check, setCheck] = useState<RecordCheck>();
  // The record's latest lines as the kernel lists them, worded with the roster's names; the
  // lines of the activity ledger where there is no signed record.
  const [lines, setLines] = useState<KernelRecordLine[]>();
  const [ledger, setLedger] = useState<RecordLine[]>([]);
  const recent = useMemo(() => (lines ? recentLines(lines, agents) : ledger), [lines, ledger, agents]);
  const [recorded, setRecorded] = useState(true);
  const [devices, setDevices] = useState<Device[]>();
  const [waiting, setWaiting] = useState<ChannelWaiting[]>([]);
  const [issued, setIssued] = useState<PairingIssued>();
  const [qr, setQr] = useState<string>();
  const [canAddPasskey, setCanAddPasskey] = useState(false);
  const [chatCode, setChatCode] = useState("");
  const [paired, setPaired] = useState("");
  const [problem, setProblem] = useState("");
  const [devicesProblem, setDevicesProblem] = useState("");
  const chatField = useId();
  const { record, devices: pairing } = seams;

  // Who can say yes is read again every few seconds: a failed read says so and keeps what was
  // shown, never an empty list that looks like nobody else can approve (spec 3.3 §5.1).
  useEffect(() => {
    if (!pairing) return;
    let live = true;
    const load = () => {
      void pairing
        .list()
        .then((value) => {
          if (!live) return;
          setDevices(value);
          setDevicesProblem("");
        })
        .catch(() => live && setDevicesProblem(labels.devicesUnread));
      void pairing.waiting?.().then((value) => live && setWaiting(value)).catch(() => undefined);
    };
    load();
    const timer = window.setInterval(load, 10_000);
    void pairing.canAddPasskey?.().then((value) => live && setCanAddPasskey(value)).catch(() => undefined);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, [pairing]);

  useEffect(() => {
    let live = true;
    // Without a signed record, what exists: the latest lines of the activity ledger.
    const fromBriefing = () =>
      client
        .briefing()
        .then((briefing: Briefing) => {
          if (!live) return;
          setRecorded(briefing.recorded !== false);
          setLedger(
            briefing.lines
              .slice(-3)
              .reverse()
              .map((line) => ({ at: line.at, atText: line.atText, text: line.text })),
          );
        })
        .catch(() => undefined);
    if (record) {
      void record
        .check()
        .then((value) => {
          if (!live) return;
          if (!value) return fromBriefing();
          setCheck(value);
          return record.lines(RECORD_READ).then((read) => live && setLines(read));
        })
        .catch((error: unknown) => live && setProblem(problemText(error)));
    } else {
      void fromBriefing();
    }
    return () => {
      live = false;
    };
  }, [client, record]);

  /** "Check again": the whole record verified now, and its latest lines read again. */
  async function checkAgain() {
    if (!record) return;
    setProblem("");
    try {
      const value = await record.check();
      if (value) setCheck(value);
      setLines(await record.lines(RECORD_READ));
    } catch (error) {
      setProblem(problemText(error));
    }
  }

  return (
    <section className="card memory-card" aria-labelledby="did-heading">
      <h2 id="did-heading" className="card-heading">
        {labels.whatTheyDid}
      </h2>
      {check && record && (
        <div className="record-check">
          <span
            className={`check-mark${check.intact ? "" : check.unsigned !== undefined ? " check-pending" : " check-broken"}`}
            aria-hidden="true"
          >
            <Icon name={check.intact ? "check" : check.unsigned !== undefined ? "dots" : "cross"} />
          </span>
          <div>
            <p className="record-title">
              {check.intact || check.unsigned !== undefined ? labels.everyActionOnRecord : labels.recordBroken}
            </p>
            {check.intact && <p className="record-sub">{labels.recordSummary(check.count, checkedWhen(check.checkedAt))}</p>}
            {!check.intact && check.unsigned !== undefined && (
              <p className="record-sub">{labels.recordUnsigned(check.count, check.unsigned, checkedWhen(check.checkedAt))}</p>
            )}
            {!check.intact && check.unsigned === undefined && check.problem && (
              <details className="record-sub">
                <summary>{labels.details}</summary>
                <p>{labels.recordBrokenHint}</p>
                <p className="mono">{check.problem}</p>
              </details>
            )}
            <button type="button" className="link link-accent" onClick={() => void checkAgain()}>
              {labels.checkAgain}
            </button>
          </div>
        </div>
      )}
      {problem && (
        <p className="decision-problem" role="alert">
          {problem}
        </p>
      )}
      {recent.length === 0 ? (
        <p className="card-empty">{recorded ? labels.recentEmpty : labels.notRecorded}</p>
      ) : (
        <ul className="bordered">
          {recent.map((line, index) => (
            <li key={line.seq ?? index} className="record-line">
              <time className="mono" dateTime={line.at}>
                {line.atText}
              </time>
              <span>{line.text}</span>
            </li>
          ))}
        </ul>
      )}
      <p className="card-label">{labels.whereYouCanSayYes}</p>
      <ul className="bordered">
        {(devices ?? []).map((device) => (
          <li key={device.id} className="device">
            <span>
              {device.name} <span className="muted">· {device.may}</span>
            </span>
            {pairing && devices && (
              <button
                type="button"
                className="link"
                onClick={() => {
                  setProblem("");
                  void pairing
                    .remove(device.id)
                    .then(async () => {
                      setDevices(await pairing.list());
                      setCanAddPasskey((await pairing.canAddPasskey?.()) ?? false);
                    })
                    .catch((error: unknown) => setProblem(problemText(error)));
                }}
              >
                {labels.remove}
              </button>
            )}
          </li>
        ))}
        <li className="device">
          <span>
            {labels.thisComputer} <span className="muted">· {labels.everything}</span>
          </span>
        </li>
      </ul>
      {devicesProblem && (
        <p className="decision-problem" role="alert">
          {devicesProblem}
        </p>
      )}
      {pairing && devices && (
        <>
          <button
            type="button"
            className="button button-primary"
            onClick={() => {
              setProblem("");
              setQr(undefined);
              void pairing
                .pair()
                .then(async (value) => {
                  setIssued(value);
                  // The QR code holds the link that opens the installed app's pairing screen (§8.3).
                  if (value.url && pairing.qr) setQr(await pairing.qr(value.url));
                })
                .catch((error: unknown) => setProblem(problemText(error)));
            }}
          >
            {labels.addPhoneOrApp}
          </button>
          {issued && (
            <div className="pairing">
              {qr && <img className="pairing-qr" src={qr} alt={phoneLabels.pairingQr} width={200} height={200} />}
              {issued.url && (
                <p className="setting-sub">
                  {phoneLabels.pairingLink} <span className="mono pairing-link">{issued.url}</span>
                </p>
              )}
              {issued.url && <p className="setting-sub">{phoneLabels.pairingCodeToType}</p>}
              <p className="pairing-code mono">{issued.code}</p>
            </div>
          )}
          {canAddPasskey && pairing.addPasskey && (
            <button
              type="button"
              className="button button-secondary"
              onClick={() => {
                setProblem("");
                void pairing.addPasskey?.()
                  .then(async () => {
                    setCanAddPasskey(false);
                    setDevices(await pairing.list());
                  })
                  .catch((error: unknown) => setProblem(problemText(error)));
              }}
            >
              {phoneLabels.useYourFaceOrFingerprintOnThisComputer}
            </button>
          )}
        </>
      )}
      {pairing?.accept && devices && (
        <form
          className="pair-chat"
          aria-labelledby={`${chatField}-heading`}
          onSubmit={(event) => {
            event.preventDefault();
            const entered = chatCode.trim();
            if (!entered || !pairing.accept) return;
            setProblem("");
            setPaired("");
            void pairing
              .accept(entered)
              .then(async (account) => {
                setChatCode("");
                // The account the kernel paired, never a guess from who asked.
                setPaired(account ? labels.paired(channelName(account.channel), accountName(account)) : labels.chatPaired);
                setDevices(await pairing.list());
                setWaiting((await pairing.waiting?.()) ?? []);
              })
              .catch((error: unknown) => setProblem(problemText(error)));
          }}
        >
          <p className="card-label" id={`${chatField}-heading`}>
            {labels.pairChat}
          </p>
          <p className="setting-sub">{labels.pairChatHint}</p>
          {waiting.map((asked) => (
            <p key={asked.id} className="setting-sub">
              {labels.waitingToPair(channelName(asked.channel), accountName(asked))}
            </p>
          ))}
          <label htmlFor={chatField} className="setting-label">
            {labels.theCodeItWasSent}
          </label>
          <div className="inline-form">
            <input
              id={chatField}
              className="mono"
              value={chatCode}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) => setChatCode(event.target.value)}
            />
            <button type="submit" className="button button-secondary" disabled={!chatCode.trim()}>
              {labels.pair}
            </button>
          </div>
          {paired && (
            <p className="change-note" role="status">
              {paired}
            </p>
          )}
        </form>
      )}
    </section>
  );
}

