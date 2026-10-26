import { useEffect, useId, useState } from "react";
import type { FormEvent } from "react";
import type {
  ConnectStep,
  DecisionView,
  DesignOption,
  FactView,
  FormView,
  OnboardingCommand,
  OnboardingView,
  PageView,
  StageLine,
  Swatch,
} from "../generated/website";
import { problemText } from "../client";
import { CharterCards } from "./Charter";
import type { WebsiteClient } from "./client";
import { websiteLabels as labels } from "./labels";
import { Display, Field, Lines, Markdown, Status, valuesOf, when } from "./parts";

type Act = (command: OnboardingCommand) => Promise<void>;

/**
 * Onboarding (spec 3.5 §7.18, §8.2): the nine stages on the left, the one to do now on the
 * right. Every input is a decision the site waits for; the page holds nothing a reload cannot
 * read again.
 */
export function Onboarding({ client, onLaunched }: { client: WebsiteClient; onLaunched: () => void }) {
  const [view, setView] = useState<OnboardingView>();
  const [shown, setShown] = useState<string>();
  const [note, setNote] = useState("");
  const [problem, setProblem] = useState("");
  const [busy, setBusy] = useState(false);
  const [next, setNext] = useState<ConnectStep>();

  useEffect(() => {
    let live = true;
    client
      .onboarding()
      .then((value) => live && setView(value))
      .catch((error: unknown) => live && setProblem(problemText(error)));
    return () => {
      live = false;
    };
  }, [client]);

  const act: Act = async (command) => {
    setBusy(true);
    setProblem("");
    setNote("");
    try {
      const outcome = await client.onboard(command);
      setView(outcome.view);
      setNote(outcome.note ?? "");
      setNext(outcome.next ?? undefined);
      // The launch check's pages are reviewed page by page before going on (spec 3.5 §7.18).
      if (command.type === "runGate") setShown("launch_gate");
      if (outcome.view.current === "done" && view?.current !== "done") onLaunched();
    } catch (error) {
      setProblem(problemText(error));
    } finally {
      setBusy(false);
    }
  };

  if (!view) {
    return (
      <main className="w-page">
        {problem ? (
          <p className="decision-problem" role="alert">
            {problem}
          </p>
        ) : (
          <p className="muted">{labels.loading}</p>
        )}
      </main>
    );
  }
  const current = shown ?? (view.current === "done" ? "run" : view.current);
  return (
    <main className="w-page w-onboarding glow">
      <Display lead={labels.onboardingLead} accent={labels.onboardingAccent}>
        {labels.onboardingSubhead}
      </Display>
      <div className="w-onboarding-grid">
        <nav className="w-stages" aria-label={labels.stages}>
          <ol>
            {view.stages.map((stage) => (
              <StageItem
                key={stage.id}
                stage={stage}
                shown={stage.id === current}
                onShow={() => setShown(stage.id === view.current ? undefined : stage.id)}
              />
            ))}
          </ol>
        </nav>
        <div className="w-stage">
          {note && (
            <p className="change-note" role="status">
              {note}
            </p>
          )}
          {problem && (
            <p className="decision-problem" role="alert">
              {problem}
            </p>
          )}
          {current === "start" && <StartStep view={view} act={act} busy={busy} />}
          {current === "discover" && <DiscoverStep view={view} act={act} busy={busy} />}
          {current === "protect" && <ProtectStep view={view} act={act} busy={busy} />}
          {current === "interview" && <InterviewStep view={view} act={act} busy={busy} />}
          {current === "charter" &&
            (view.charter ? (
              <CharterCards
                charter={view.charter}
                busy={busy}
                onCommand={(command) => void act({ type: "charter", command })}
              />
            ) : (
              <p className="muted">{labels.charterLater}</p>
            ))}
          {current === "build" && <DesignStep view={view} act={act} busy={busy} />}
          {current === "launch_gate" && <GateStep view={view} act={act} busy={busy} />}
          {current === "connect_and_deploy" && <ConnectStep view={view} act={act} busy={busy} next={next} />}
          {current === "run" && <RunStep view={view} />}
        </div>
      </div>
    </main>
  );
}

function StageItem({ stage, shown, onShow }: { stage: StageLine; shown: boolean; onShow: () => void }) {
  const reachable = stage.state !== "later";
  return (
    <li className={`w-stage-item w-stage-${stage.state}`} aria-current={shown ? "step" : undefined}>
      <span className="w-stage-number" aria-hidden="true">
        {stage.state === "done" ? "✓" : stage.number}
      </span>
      {reachable ? (
        <button type="button" className="w-stage-link" onClick={onShow}>
          {stage.title}
        </button>
      ) : (
        <span className="w-stage-link">{stage.title}</span>
      )}
      <span className="visually-hidden">
        {stage.state === "done" ? labels.stageDone : stage.state === "current" ? labels.stageNow : ""}
      </span>
    </li>
  );
}

function StartStep({ view, act, busy }: { view: OnboardingView; act: Act; busy: boolean }) {
  const [from, setFrom] = useState<"site" | "presence" | "nothing">("nothing");
  const [url, setUrl] = useState("");
  const [profiles, setProfiles] = useState("");
  const [name, setName] = useState(view.siteName);
  const [phone, setPhone] = useState("");
  const ids = { url: useId(), profiles: useId(), name: useId(), phone: useId() };
  if (view.start.started) {
    return (
      <section className="card w-step">
        <h2 className="card-heading">{labels.startHeading}</h2>
        <p>{view.start.from}</p>
      </section>
    );
  }
  function submit(event: FormEvent) {
    event.preventDefault();
    void act({
      type: "start",
      from,
      url: from === "site" ? url : null,
      profiles: from === "presence" ? profiles.split("\n").map((p) => p.trim()).filter(Boolean) : [],
      name,
      phone: phone || null,
    });
  }
  return (
    <form className="card w-step w-form" onSubmit={submit}>
      <h2 className="card-heading">{labels.startHeading}</h2>
      <div className="segmented w-segmented-3" role="radiogroup" aria-label={labels.startHeading}>
        {(
          [
            ["site", labels.startSite],
            ["presence", labels.startPresence],
            ["nothing", labels.startNothing],
          ] as const
        ).map(([value, label]) => (
          <button
            key={value}
            type="button"
            role="radio"
            aria-checked={from === value}
            className="segment"
            onClick={() => setFrom(value)}
          >
            {label}
          </button>
        ))}
      </div>
      {from === "site" && (
        <div className="w-field">
          <label htmlFor={ids.url}>{labels.siteUrl}</label>
          <input id={ids.url} type="url" placeholder="https://" value={url} onChange={(e) => setUrl(e.target.value)} />
        </div>
      )}
      {from === "presence" && (
        <div className="w-field">
          <label htmlFor={ids.profiles}>{labels.profiles}</label>
          <textarea id={ids.profiles} rows={3} value={profiles} onChange={(e) => setProfiles(e.target.value)} />
        </div>
      )}
      <div className="w-field">
        <label htmlFor={ids.name}>{labels.siteName}</label>
        <input id={ids.name} value={name} onChange={(e) => setName(e.target.value)} />
      </div>
      <div className="w-field">
        <label htmlFor={ids.phone}>{labels.phone}</label>
        <input id={ids.phone} type="tel" value={phone} onChange={(e) => setPhone(e.target.value)} aria-describedby={`${ids.phone}-hint`} />
        <p id={`${ids.phone}-hint`} className="w-hint">
          {labels.phoneHint}
        </p>
      </div>
      <div className="w-actions">
        <button type="submit" className="button button-primary" disabled={busy}>
          {labels.start}
        </button>
      </div>
    </form>
  );
}

function host(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

function FactRow({ fact, act, busy }: { fact: FactView; act: Act; busy: boolean }) {
  const [fixing, setFixing] = useState(false);
  const [text, setText] = useState(fact.text);
  const id = useId();
  return (
    <li className="fact">
      <p className="fact-text">{fact.text}</p>
      <p className="fact-meta">
        <Status pill={fact.status} />
        {fact.source && (
          <a href={fact.source} target="_blank" rel="noreferrer" className="muted">
            {host(fact.source)}
          </a>
        )}
      </p>
      {fact.open && !fixing && (
        <p className="row-actions">
          <button type="button" className="link link-accent" disabled={busy} onClick={() => void act({ type: "fact", id: fact.id, answer: "right" })}>
            {labels.thatsRight}
          </button>
          <button type="button" className="link" disabled={busy} onClick={() => setFixing(true)}>
            {labels.fixIt}
          </button>
          <button type="button" className="link" disabled={busy} onClick={() => void act({ type: "fact", id: fact.id, answer: "forget" })}>
            {labels.forgetIt}
          </button>
        </p>
      )}
      {fixing && (
        <form
          className="inline-form"
          onSubmit={(event) => {
            event.preventDefault();
            setFixing(false);
            void act({ type: "fact", id: fact.id, answer: "fix", text });
          }}
        >
          <label htmlFor={id} className="visually-hidden">
            {labels.fixPlaceholder}
          </label>
          <input id={id} value={text} onChange={(e) => setText(e.target.value)} />
          <button type="submit" className="button button-secondary" disabled={!text.trim()}>
            {labels.save}
          </button>
        </form>
      )}
    </li>
  );
}

function DecisionCardView({ decision, act, busy }: { decision: DecisionView; act: Act; busy: boolean }) {
  return (
    <article className="decision w-decision">
      <h3 className="w-section-title">{decision.title}</h3>
      <p>{decision.detail}</p>
      {decision.evidence.length > 0 && (
        <ul className="w-evidence">
          {decision.evidence.map((line) => (
            <li key={line} className="mono">
              {line}
            </li>
          ))}
        </ul>
      )}
      <div className="w-actions">
        {decision.options.map((option) => (
          <button
            key={option.value}
            type="button"
            className={`button ${(decision.chosen ?? decision.default) === option.value ? "button-primary" : "button-secondary"}`}
            aria-pressed={decision.chosen === option.value}
            disabled={busy}
            onClick={() => void act({ type: "decide", card: decision.id, choice: option.value })}
          >
            {option.label}
          </button>
        ))}
      </div>
    </article>
  );
}

function DiscoverStep({ view, act, busy }: { view: OnboardingView; act: Act; busy: boolean }) {
  const discovery = view.discovery;
  const [kind, setKind] = useState(discovery.kind.confirmed ?? discovery.kind.proposed ?? "");
  const kindId = useId();
  const proposed = discovery.kind.options.find((option) => option.value === discovery.kind.proposed);
  return (
    <div className="w-step-stack">
      <section className="card w-step">
        <h2 className="card-heading">{labels.foundHeading}</h2>
        {discovery.ran ? (
          <>
            {discovery.source && <p className="muted">{discovery.source}</p>}
            {discovery.found.length > 0 && <Lines lines={discovery.found} className="w-lines w-lines-inline" />}
            <p className="w-note">{labels.aGuessNote}</p>
            <ul className="bordered">
              {discovery.facts.map((fact) => (
                <FactRow key={fact.id} fact={fact} act={act} busy={busy} />
              ))}
            </ul>
          </>
        ) : (
          <p className="muted">{labels.foundNothing}</p>
        )}
      </section>
      <section className="decision w-step" aria-labelledby={kindId}>
        <h2 id={kindId} className="card-heading">
          {labels.kindHeading}
        </h2>
        {proposed && (
          <p>
            {labels.kindProposed} <strong>{proposed.label.toLowerCase()}</strong>.{" "}
            {discovery.kind.reasons.length > 0 && (
              <span className="muted">
                {labels.because} {discovery.kind.reasons.join("; ")}.
              </span>
            )}
          </p>
        )}
        <select aria-label={labels.chooseKind} value={kind} onChange={(e) => setKind(e.target.value)} disabled={!!discovery.kind.confirmed}>
          <option value="">—</option>
          {discovery.kind.options.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>
        {discovery.kind.confirmed ? (
          <p className="change-note">{discovery.kind.options.find((o) => o.value === discovery.kind.confirmed)?.label}</p>
        ) : (
          <div className="w-actions">
            <button type="button" className="button button-primary" disabled={busy || !kind} onClick={() => void act({ type: "kind", kind })}>
              {labels.confirmKind}
            </button>
          </div>
        )}
      </section>
      {discovery.decisions.length > 0 && (
        <section className="w-step-stack" aria-label={labels.decisionsHeading}>
          <h2 className="card-heading">{labels.decisionsHeading}</h2>
          {discovery.decisions.map((decision) => (
            <DecisionCardView key={decision.id} decision={decision} act={act} busy={busy} />
          ))}
        </section>
      )}
    </div>
  );
}

function ProtectStep({ view, act, busy }: { view: OnboardingView; act: Act; busy: boolean }) {
  const protect = view.protect;
  if (!protect) {
    return <p className="card w-step">{labels.noOldSite}</p>;
  }
  return (
    <section className="decision w-step">
      <h2 className="card-heading">{labels.protectHeading}</h2>
      <p className="mono">{protect.url}</p>
      <p>{protect.text}</p>
      <div className="w-actions">
        <button type="button" className="button button-primary" disabled={busy} onClick={() => void act({ type: "protect", freeze: true })}>
          {labels.freeze}
        </button>
        <button type="button" className="button button-secondary" disabled={busy} onClick={() => void act({ type: "protect", freeze: false })}>
          {labels.notNow}
        </button>
      </div>
    </section>
  );
}

function FormCard({ form, act, busy, locked }: { form: FormView; act: Act; busy: boolean; locked: boolean }) {
  const [open, setOpen] = useState(!form.answered);
  const [values, setValues] = useState<Record<string, string>>(() => valuesOf(form.fields));
  useEffect(() => setOpen(!form.answered), [form.answered]);
  function submit(event: FormEvent) {
    event.preventDefault();
    void act({ type: "answer", form: form.id, values });
  }
  return (
    <article className={`card w-question${form.answered ? " w-question-answered" : ""}`}>
      <div className="w-section-head">
        <h3 className="w-section-title">{form.question}</h3>
        {form.answered && <span className="pill pill-decided">{form.fromDiscovery ? labels.fromDiscovery : labels.answered}</span>}
      </div>
      {open && !locked ? (
        <form className="w-form" onSubmit={submit}>
          {form.fields.map((field) => (
            <Field key={field.id} spec={field} value={values[field.id] ?? ""} onChange={(value) => setValues((all) => ({ ...all, [field.id]: value }))} />
          ))}
          <div className="w-actions">
            <button type="submit" className="button button-primary" disabled={busy}>
              {labels.answer}
            </button>
          </div>
        </form>
      ) : (
        <>
          <Lines
            lines={form.fields
              .filter((field) => field.value)
              .map((field) => ({
                label: field.label,
                value: field.options?.find((o) => o.value === field.value)?.label ?? field.value ?? "",
              }))}
          />
          {!locked && (
            <button type="button" className="link" onClick={() => setOpen(true)}>
              {labels.change}
            </button>
          )}
        </>
      )}
    </article>
  );
}

function InterviewStep({ view, act, busy }: { view: OnboardingView; act: Act; busy: boolean }) {
  const interview = view.interview;
  if (interview.forms.length === 0) {
    return <p className="card w-step">{labels.kindFirst}</p>;
  }
  const preview = interview.preview;
  return (
    <div className="w-interview">
      <div className="w-step-stack">
        <h2 className="card-heading">{labels.interviewHeading}</h2>
        {interview.forms.map((form) => (
          <FormCard key={form.id} form={form} act={act} busy={busy} locked={interview.done} />
        ))}
        {!interview.done && (
          <div className="w-actions">
            <button type="button" className="button button-primary" disabled={busy} onClick={() => void act({ type: "finishInterview" })}>
              {labels.finishInterview}
            </button>
          </div>
        )}
      </div>
      <aside className="w-preview" aria-label={labels.previewHeading}>
        <p className="card-label">{labels.previewHeading}</p>
        {preview ? (
          <div className="w-browser">
            <div className="w-browser-bar" aria-hidden="true">
              <i />
              <i />
              <i />
            </div>
            <div className="w-browser-page">
              <p className="w-browser-name">{preview.siteName}</p>
              <ul className="w-browser-nav">
                {preview.pages.map((page) => (
                  <li key={page.title}>{page.title}</li>
                ))}
              </ul>
              {preview.about ? <Markdown>{preview.about}</Markdown> : <p className="muted">…</p>}
            </div>
            <p className="w-browser-foot muted">
              {labels.previewRebuilt} {when(preview.rebuiltAt)} · {preview.facts} {labels.previewFacts}
            </p>
          </div>
        ) : (
          <p className="muted">{labels.previewLater}</p>
        )}
      </aside>
    </div>
  );
}

function Swatches({ swatch, label }: { swatch: Swatch; label: string }) {
  return (
    <div className="w-swatch" style={{ background: swatch.background, borderColor: swatch.border }} aria-label={label} role="img">
      <span style={{ color: swatch.text }}>Aa</span>
      <i style={{ background: swatch.accent }} />
      <i style={{ background: swatch.link }} />
    </div>
  );
}

function DesignCard({ option, chosen, act, busy }: { option: DesignOption; chosen: boolean; act: Act; busy: boolean }) {
  return (
    <article className={`card w-design${chosen ? " w-design-chosen" : ""}`}>
      <h3 className="w-section-title">{option.name}</h3>
      {option.light && option.dark ? (
        <div className="w-swatches">
          <Swatches swatch={option.light} label={`${option.name}, ${labels.light}`} />
          <Swatches swatch={option.dark} label={`${option.name}, ${labels.dark}`} />
        </div>
      ) : (
        <div className="w-swatches w-swatches-current" aria-hidden="true">
          <span>{labels.currentDesign}</span>
        </div>
      )}
      <p className="muted">{option.direction}</p>
      {chosen ? (
        <span className="pill pill-decided">{labels.chosen}</span>
      ) : (
        <button type="button" className="button button-secondary" disabled={busy} onClick={() => void act({ type: "design", design: option.id })}>
          {labels.choose}
        </button>
      )}
    </article>
  );
}

function DesignStep({ view, act, busy }: { view: OnboardingView; act: Act; busy: boolean }) {
  const design = view.design;
  return (
    <div className="w-step-stack">
      <div>
        <h2 className="card-heading">{labels.designHeading}</h2>
        <p className="muted">{labels.designSubhead}</p>
      </div>
      <div className="w-design-grid">
        {design.options.map((option) => (
          <DesignCard key={option.id} option={option} chosen={design.chosen === option.id} act={act} busy={busy} />
        ))}
      </div>
      {design.built && <p className="change-note">{design.built}</p>}
      {design.held.length > 0 && (
        <ul className="w-problems">
          {design.held.map((item) => (
            <li key={item}>{item}</li>
          ))}
        </ul>
      )}
    </div>
  );
}

function PageRow({ page }: { page: PageView }) {
  const [open, setOpen] = useState(false);
  return (
    <li className="w-page-row">
      <div className="w-page-line">
        <Status pill={page.change} />
        <span className="w-page-title">{page.title}</span>
        <span className="mono muted">{page.path}</span>
        {page.markdown && (
          <button type="button" className="link" aria-expanded={open} onClick={() => setOpen(!open)}>
            {open ? labels.hidePage : labels.showPage}
          </button>
        )}
      </div>
      {open && page.markdown && <Markdown>{page.markdown}</Markdown>}
    </li>
  );
}

function GateStep({ view, act, busy }: { view: OnboardingView; act: Act; busy: boolean }) {
  const gate = view.gate;
  return (
    <div className="w-step-stack">
      <div className="w-section-head">
        <div>
          <h2 className="card-heading">{labels.gateHeading}</h2>
          <p className="muted">{labels.gateSubhead}</p>
        </div>
        <button type="button" className="button button-secondary" disabled={busy} onClick={() => void act({ type: "runGate" })}>
          {gate ? labels.runAgain : labels.runGate}
        </button>
      </div>
      {gate && (
        <>
          <p className={gate.passed ? "change-note" : "decision-problem"}>{gate.text}</p>
          <ul className="bordered w-checks">
            {gate.checks.map((check) => (
              <li key={check.name}>
                <div className="w-check-line">
                  <span>{check.name}</span>
                  <Status pill={check.status} />
                </div>
                {check.findings.map((finding) => (
                  <p key={finding} className="w-hint">
                    {finding}
                  </p>
                ))}
                {check.note && <p className="w-hint">{check.note}</p>}
              </li>
            ))}
          </ul>
          <h3 className="card-label">{labels.pages}</h3>
          <ul className="bordered">
            {gate.pages.map((page) => (
              <PageRow key={page.path} page={page} />
            ))}
          </ul>
        </>
      )}
    </div>
  );
}

function ConnectStep({ view, act, busy, next }: { view: OnboardingView; act: Act; busy: boolean; next?: ConnectStep }) {
  const deploy = view.deploy;
  return (
    <div className="w-step-stack">
      <h2 className="card-heading">{labels.connectHeading}</h2>
      <p className="always-asks">{view.connections.notice}</p>
      {next && next.type !== "done" && (
        <p className="change-note" role="status">
          {next.type === "open" ? (
            <a href={next.url} target="_blank" rel="noreferrer">
              {labels.openPage}: {host(next.url)}
            </a>
          ) : (
            <>
              {next.how}: <strong className="mono">{next.code}</strong>
            </>
          )}
        </p>
      )}
      <ul className="bordered w-connections">
        {view.connections.cards.map((card) => (
          <li key={card.id} className="w-connection">
            <div>
              <p className="w-connection-kind">{card.kind}</p>
              <p>{card.service}</p>
              <p className="muted">{card.account ?? card.method}</p>
            </div>
            <Status pill={card.status} />
            {card.canConnect && (
              <div className="w-actions">
                <button type="button" className="button button-secondary" disabled={busy} onClick={() => void act({ type: "connect", connection: card.id })}>
                  {labels.connect}
                </button>
                <button type="button" className="link" disabled={busy} onClick={() => void act({ type: "skip", connection: card.id })}>
                  {labels.later}
                </button>
              </div>
            )}
          </li>
        ))}
      </ul>
      <section className="decision w-step" aria-label={labels.deployHeading}>
        <h3 className="card-heading">{labels.deployHeading}</h3>
        <p className="muted">{deploy.shape}</p>
        {deploy.cost.length > 0 && (
          <>
            <p className="card-label">{labels.runningCost}</p>
            <Lines lines={deploy.cost} />
            {deploy.fits !== undefined && deploy.fits !== null && (
              <span className={`pill ${deploy.fits ? "pill-decided" : "pill-waiting"}`}>{deploy.fits ? labels.fitsBudget : labels.overBudget}</span>
            )}
          </>
        )}
        {deploy.live ? (
          <p className="change-note">{deploy.live}</p>
        ) : deploy.waiting ? (
          <div className="w-approvals">
            <p>{deploy.waiting.text}</p>
            <ul className="w-signer-list">
              {deploy.waiting.lines.map((line, index) => (
                <li key={`${line.role}-${index}`} className="w-signer">
                  <span className="w-signer-role">{line.label}</span>
                  <span className="muted">{line.holders.join(", ")}</span>
                  <span className={`pill ${line.signedBy ? "pill-decided" : "pill-waiting"}`}>{line.signedBy ?? labels.waitingSignature}</span>
                </li>
              ))}
            </ul>
            <div className="w-actions">
              {deploy.waiting.canApprove.map((role) => (
                <button key={role} type="button" className="button button-primary" disabled={busy} onClick={() => void act({ type: "approve", role })}>
                  {labels.approveAs} {role}
                </button>
              ))}
            </div>
          </div>
        ) : (
          <div className="w-actions">
            <button type="button" className="button button-primary" disabled={busy || !deploy.canGoLive} onClick={() => void act({ type: "goLive" })}>
              {labels.goLive}
            </button>
          </div>
        )}
      </section>
    </div>
  );
}

function RunStep({ view }: { view: OnboardingView }) {
  const run = view.run;
  if (!run) return <p className="card w-step">{labels.planLater}</p>;
  return (
    <div className="w-step-stack">
      <h2 className="card-heading">{labels.runHeading}</h2>
      <Lines
        lines={[
          { label: labels.firstReview, value: when(run.firstReview) },
          { label: labels.suggestsUntil, value: when(run.proposeUntil) },
        ]}
      />
      <div className="w-phases">
        {run.phases.map((phase, index) => (
          <section key={labels.phases[index]} className="card">
            <h3 className="w-section-title">{labels.phases[index]}</h3>
            <ul className="w-bullets">
              {phase.map((line) => (
                <li key={line}>{line}</li>
              ))}
            </ul>
          </section>
        ))}
      </div>
      <section className="card">
        <h3 className="w-section-title">{labels.levelsOffered}</h3>
        <Lines lines={run.promotions} />
      </section>
    </div>
  );
}
