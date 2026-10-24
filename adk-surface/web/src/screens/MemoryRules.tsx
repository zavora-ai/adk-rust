import { useEffect, useId, useState } from "react";
import type { Briefing, Mode, StatusLine } from "../generated/api";
import type { HomeClient } from "../client";
import { problemText } from "../client";
import { Icon } from "../components/Icon";
import { Pill } from "../components/Pill";
import { dayMonth, dollars, hhmm } from "../format";
import type { StandingRule } from "../kernel";
import { allows } from "../kernel";
import { labels, modes } from "../labels";
import type { Device, MemoryFact, RecordCheck, RecordLine, Seams, TidyUp } from "../seams";

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
  seams,
  onChanged,
}: {
  client: HomeClient;
  status?: StatusLine;
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
        <Remember seams={seams} />
        <CanDo client={client} status={status} seams={seams} onChanged={onChanged} />
        <Did client={client} seams={seams} />
      </div>
    </main>
  );
}

function Remember({ seams }: { seams: Seams }) {
  const [facts, setFacts] = useState<MemoryFact[]>([]);
  const [tidyUp, setTidyUp] = useState<TidyUp>();
  const [problem, setProblem] = useState("");
  const [fixing, setFixing] = useState<string>();
  const [fixText, setFixText] = useState("");
  const fixField = useId();
  const memory = seams.memory;

  const load = () =>
    memory
      ?.facts()
      .then((read) => {
        setFacts(read.facts);
        setTidyUp(read.tidyUp);
      })
      .catch((error: unknown) => setProblem(problemText(error)));
  useEffect(() => {
    void load();
  }, [memory]);

  async function act(action: () => Promise<void>) {
    setProblem("");
    try {
      await action();
      await load();
    } catch (error) {
      setProblem(problemText(error));
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
          <button type="button" className="button button-secondary" onClick={() => void act(() => memory.undo(tidyUp.id))}>
            {labels.undo}
          </button>
        </div>
      )}
      {problem && (
        <p className="decision-problem" role="alert">
          {problem}
        </p>
      )}
      {facts.length === 0 ? (
        <p className="card-empty">{labels.rememberEmpty}</p>
      ) : (
        <ul className="bordered">
          {facts.map((fact) => (
            <li key={fact.id} className="fact">
              <p className="fact-text">{fact.text}</p>
              <p className="fact-meta">
                <Pill kind={provenance[fact.provenance].kind}>{provenance[fact.provenance].text}</Pill>
                {fact.provenance === "aGuess" && fact.noticedBy && <span>{labels.noticedThis(fact.noticedBy)}</span>}
              </p>
              {fact.provenance === "aGuess" && memory && fixing !== fact.id && (
                <p className="row-actions">
                  <button type="button" className="link link-accent" onClick={() => void act(() => memory.confirm(fact.id))}>
                    {labels.thatsRight}
                  </button>
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
                  <button type="button" className="link" onClick={() => void act(() => memory.forget(fact.id))}>
                    {labels.forgetIt}
                  </button>
                </p>
              )}
              {fixing === fact.id && memory && (
                <form
                  className="inline-form"
                  onSubmit={(event) => {
                    event.preventDefault();
                    void act(() => memory.fix(fact.id, fixText)).then(() => setFixing(undefined));
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Escape") setFixing(undefined);
                  }}
                >
                  <label htmlFor={fixField} className="visually-hidden">
                    {labels.fixIt}
                  </label>
                  <input id={fixField} value={fixText} onChange={(event) => setFixText(event.target.value)} autoFocus />
                  <button type="submit" className="button button-secondary">
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
  seams,
  onChanged,
}: {
  client: HomeClient;
  status?: StatusLine;
  seams: Seams;
  onChanged: () => void;
}) {
  const [rules, setRules] = useState<StandingRule[]>();
  const [problem, setProblem] = useState("");
  const limits = seams.limits;
  const current = status?.mode?.mode;

  const load = () => client.rules().then(setRules).catch((error: unknown) => setProblem(problemText(error)));
  useEffect(() => {
    void load();
  }, [client]);

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

  const allowed = (rules ?? []).filter(allows);
  const quiet = status?.quiet ? `${status.quiet.start}–${status.quiet.end}` : labels.notSet;
  const limit =
    status?.spend?.limitMicroUsd !== undefined && status?.spend?.limitMicroUsd !== null
      ? dollars(status.spend.limitMicroUsd)
      : labels.notSet;

  return (
    <section className="card memory-card" aria-labelledby="can-do-heading">
      <h2 id="can-do-heading" className="card-heading">
        {labels.whatTheyCanDo}
      </h2>
      {current && (
        <>
          <p className="card-label" id="mode-label">
            {labels.yourAgentsMode}
          </p>
          {limits ? (
            <div className="segmented" role="radiogroup" aria-labelledby="mode-label">
              {modes.map(({ mode, label }) => (
                <button
                  key={mode}
                  type="button"
                  role="radio"
                  aria-checked={mode === current}
                  className="segment"
                  onClick={() => void act(() => limits.setMode(mode as Mode))}
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
                <li key={rule.id} className="rule">
                  <p className="rule-text">{rule.label}</p>
                  <p className="row-actions">
                    <span className="muted">
                      {rule.expiresAt ? labels.until(dayMonth(new Date(rule.expiresAt))) : labels.noEndDate}
                    </span>
                    <button type="button" className="link" onClick={() => void act(() => client.revokeRule(rule.id))}>
                      {labels.stopAllowing}
                    </button>
                  </p>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
      <p className="always-asks">{labels.alwaysAsks}</p>
      <ul className="bordered settings">
        <Setting
          label={labels.quietTime}
          value={quiet}
          onChange={
            limits
              ? (value) => {
                  const [start, end] = value.split(/[–-]/).map((part) => part.trim());
                  return act(() => limits.setQuietTime(start ?? "", end ?? ""));
                }
              : undefined
          }
        />
        <Setting
          label={labels.dailySpendingLimit}
          value={limit}
          onChange={
            limits
              ? (value) => {
                  const amount = Number(value.replace(/[$,\s]/g, ""));
                  return act(() => limits.setDailyLimit(Number.isFinite(amount) ? Math.round(amount * 1_000_000) : null));
                }
              : undefined
          }
        />
      </ul>
    </section>
  );
}

function Setting({ label, value, onChange }: { label: string; value: string; onChange?: (value: string) => Promise<void> }) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(value);
  const field = useId();
  return (
    <li className="setting">
      <div>
        <label className="setting-label" htmlFor={field}>
          {label}
        </label>
        {editing && onChange ? (
          <form
            className="inline-form"
            onSubmit={(event) => {
              event.preventDefault();
              void onChange(text).then(() => setEditing(false));
            }}
            onKeyDown={(event) => {
              if (event.key === "Escape") setEditing(false);
            }}
          >
            <input id={field} value={text} onChange={(event) => setText(event.target.value)} autoFocus />
            <button type="submit" className="button button-secondary">
              {labels.change}
            </button>
          </form>
        ) : (
          <p className="setting-value" id={field}>
            {value}
          </p>
        )}
      </div>
      {onChange && !editing && (
        <button
          type="button"
          className="link link-accent"
          onClick={() => {
            setText(value);
            setEditing(true);
          }}
        >
          {labels.change}
        </button>
      )}
    </li>
  );
}

/** "just now" within a minute of the check, else "at 07:31". */
function checkedWhen(at: string): string {
  const when = new Date(at);
  return Date.now() - when.getTime() < 60_000 ? labels.justNow : labels.atTime(hhmm(when));
}

function Did({ client, seams }: { client: HomeClient; seams: Seams }) {
  const [check, setCheck] = useState<RecordCheck>();
  const [recent, setRecent] = useState<RecordLine[]>([]);
  const [devices, setDevices] = useState<Device[]>();
  const [code, setCode] = useState("");
  const [problem, setProblem] = useState("");
  const { record, devices: pairing } = seams;

  useEffect(() => {
    let live = true;
    if (record) {
      void record.check().then((value) => live && setCheck(value)).catch((error: unknown) => live && setProblem(problemText(error)));
      void record.recent().then((value) => live && setRecent(value)).catch(() => undefined);
    } else {
      // What exists: the latest lines of the activity ledger, as sentences.
      void client
        .briefing()
        .then((briefing: Briefing) => {
          if (!live) return;
          setRecent(
            briefing.lines
              .slice(-3)
              .reverse()
              .map((line) => ({ at: line.at, atText: line.atText, text: line.text })),
          );
        })
        .catch(() => undefined);
    }
    void pairing?.list().then((value) => live && setDevices(value)).catch(() => undefined);
    return () => {
      live = false;
    };
  }, [client, record, pairing]);

  return (
    <section className="card memory-card" aria-labelledby="did-heading">
      <h2 id="did-heading" className="card-heading">
        {labels.whatTheyDid}
      </h2>
      {check && record && (
        <div className="record-check">
          <span className={`check-mark${check.intact ? "" : " check-broken"}`} aria-hidden="true">
            <Icon name={check.intact ? "check" : "cross"} />
          </span>
          <div>
            <p className="record-title">{check.intact ? labels.everyActionOnRecord : labels.recordBroken}</p>
            {check.intact && <p className="record-sub">{labels.recordSummary(check.count, checkedWhen(check.checkedAt))}</p>}
            <button
              type="button"
              className="link link-accent"
              onClick={() => void record.check().then(setCheck).catch((error: unknown) => setProblem(problemText(error)))}
            >
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
        <p className="card-empty">{labels.recentEmpty}</p>
      ) : (
        <ul className="bordered">
          {recent.map((line, index) => (
            <li key={index} className="record-line">
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
                    .then(() => pairing.list().then(setDevices))
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
      {pairing && devices && (
        <>
          <button
            type="button"
            className="button button-primary"
            onClick={() => {
              setProblem("");
              void pairing
                .pair()
                .then(setCode)
                .catch((error: unknown) => setProblem(problemText(error)));
            }}
          >
            {labels.addPhoneOrApp}
          </button>
          {code && <p className="pairing-code mono">{code}</p>}
        </>
      )}
    </section>
  );
}
