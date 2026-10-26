import { useEffect, useId, useState } from "react";
import type { JobLine, ReleaseLine, SiteCommand, SiteViewModel } from "../generated/website";
import { problemText } from "../client";
import type { WebsiteClient } from "./client";
import { websiteLabels as labels } from "./labels";
import { Display, Status, when } from "./parts";

/**
 * The site view (spec 3.5 §7.24, §8.2): status, releases and the pages each changed, KPIs with
 * their trends, the jobs and how much each does alone, signals, the wallet and what is held.
 * Promoting a job, lowering one and deciding what is held are the owner's decisions (A-I13,
 * A-I27).
 */
export function SiteView({ client }: { client: WebsiteClient }) {
  const [view, setView] = useState<SiteViewModel>();
  const [note, setNote] = useState("");
  const [problem, setProblem] = useState("");
  const [busy, setBusy] = useState(false);
  const [allJobs, setAllJobs] = useState(false);

  useEffect(() => {
    let live = true;
    client
      .site()
      .then((value) => live && setView(value))
      .catch((error: unknown) => live && setProblem(problemText(error)));
    return () => {
      live = false;
    };
  }, [client]);

  async function decide(command: SiteCommand) {
    setBusy(true);
    setNote("");
    setProblem("");
    try {
      const outcome = await client.decideSite(command);
      setView(outcome.view);
      setNote(outcome.note ?? "");
    } catch (error) {
      setProblem(problemText(error));
    } finally {
      setBusy(false);
    }
  }

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
  const latest = view.releases[0];
  const held = view.held.items.length + view.held.deliveries.length + (view.wallet?.held.length ?? 0);
  return (
    <main className="w-page glow">
      <Display lead={labels.siteLead} accent={view.name}>
        <a href={view.origin} target="_blank" rel="noreferrer" className="mono w-origin">
          {view.origin.replace(/^https?:\/\//, "")}
        </a>
      </Display>
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
      <div className="stats w-stats">
        <div className="stat">
          <span className="stat-label">{labels.status}</span>
          <span className="stat-value stat-value-small">
            <Status pill={view.status.state} />
          </span>
          {view.status.since && <span className="muted w-stat-sub">{labels.since} {when(view.status.since)}</span>}
        </div>
        <div className="stat">
          <span className="stat-label">{labels.release}</span>
          <span className="stat-value">{latest ? latest.seq : "—"}</span>
          {latest && <span className="muted w-stat-sub">{when(latest.at)} · {labels.signedRelease}</span>}
        </div>
        <div className="stat">
          <span className="stat-label">{labels.signals}</span>
          <span className="stat-value">{view.signals.open}</span>
        </div>
        <div className="stat">
          <span className="stat-label">{labels.heldHeading}</span>
          <span className={`stat-value${held > 0 ? " stat-accent" : ""}`}>{held}</span>
        </div>
      </div>

      {view.alarms.length > 0 && (
        <section className="w-band" aria-label={labels.alarms}>
          {view.alarms.map((alarm) => (
            <div key={alarm.id} className="decision w-alarm">
              <Status pill={alarm.urgency} />
              <span>{alarm.text}</span>
              <span className="muted">{when(alarm.raisedAt)}</span>
              {alarm.canAcknowledge && (
                <button type="button" className="button button-secondary" disabled={busy} onClick={() => void decide({ type: "acknowledge", alarm: alarm.id })}>
                  {labels.acknowledge}
                </button>
              )}
            </div>
          ))}
        </section>
      )}

      <div className="w-site-grid">
        <div className="w-site-main">
          <section className="card" aria-labelledby="kpi-heading">
            <h2 id="kpi-heading" className="card-heading">
              {labels.measures}
            </h2>
            {view.kpis.length === 0 ? (
              <p className="card-empty">{labels.noMeasures}</p>
            ) : (
              <table className="w-table">
                <thead>
                  <tr>
                    <th scope="col">{labels.measure}</th>
                    <th scope="col">{labels.thisWeek}</th>
                    <th scope="col">{labels.lastWeek}</th>
                    <th scope="col">{labels.baseline}</th>
                    <th scope="col">{labels.target}</th>
                    <th scope="col">{labels.trend}</th>
                  </tr>
                </thead>
                <tbody>
                  {view.kpis.map((kpi) => (
                    <tr key={kpi.id}>
                      <th scope="row">
                        {kpi.name}
                        <span className="w-hint">{kpi.phase}</span>
                      </th>
                      <td className="mono">{kpi.thisWeek ?? "—"}</td>
                      <td className="mono">{kpi.lastWeek ?? "—"}</td>
                      <td className="mono">{kpi.baseline ?? "—"}</td>
                      <td className="mono">{kpi.target ?? "—"}</td>
                      <td>
                        <Status pill={kpi.trend} />
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </section>

          <section className="card" aria-labelledby="jobs-heading">
            <h2 id="jobs-heading" className="card-heading">
              {labels.jobs}
            </h2>
            <p className="w-note">{labels.jobsNote}</p>
            <ul className="bordered w-jobs">
              {[...view.jobs.filter((job) => job.offered), ...view.jobs.filter((job) => !job.offered).slice(0, allJobs ? undefined : JOBS_SHOWN)].map((job) => (
                <JobRow key={job.id} job={job} busy={busy} decide={decide} />
              ))}
            </ul>
            {view.jobs.filter((job) => !job.offered).length > JOBS_SHOWN && (
              <button type="button" className="link w-more" onClick={() => setAllJobs(!allJobs)}>
                {allJobs ? labels.fewerJobs : labels.allJobs(view.jobs.length)}
              </button>
            )}
          </section>

          <section className="card" aria-labelledby="releases-heading">
            <h2 id="releases-heading" className="card-heading">
              {labels.releases}
            </h2>
            {view.releases.length === 0 ? (
              <p className="card-empty">{labels.noReleases}</p>
            ) : (
              <ul className="bordered">
                {view.releases.map((release) => (
                  <ReleaseRow key={release.seq} release={release} />
                ))}
              </ul>
            )}
          </section>
        </div>

        <aside className="w-site-side">
          <section className="card" aria-labelledby="held-heading">
            <h2 id="held-heading" className="card-heading">
              {labels.heldHeading}
            </h2>
            <p className="w-note">{labels.heldNote}</p>
            {view.held.items.length === 0 && view.held.deliveries.length === 0 && <p className="card-empty">{labels.nothingHeld}</p>}
            {view.held.items.map((item) => (
              <div key={item.id} className="decision w-held">
                <strong>{item.title}</strong>
                <span>{item.reason}</span>
                <span className="muted">
                  Batch {item.batch} · {when(item.heldAt)}
                </span>
                <div className="w-actions">
                  <button type="button" className="button button-secondary" disabled={busy} onClick={() => void decide({ type: "item", id: item.id, decision: "release" })}>
                    {labels.release_}
                  </button>
                  <button type="button" className="button button-quiet" disabled={busy} onClick={() => void decide({ type: "item", id: item.id, decision: "discard" })}>
                    {labels.discard}
                  </button>
                </div>
              </div>
            ))}
            {view.held.deliveries.map((delivery) => (
              <div key={delivery.delivery} className="decision w-held">
                <strong>
                  {delivery.feed} · delivery {delivery.delivery}
                </strong>
                {delivery.breaks.map((line) => (
                  <span key={line}>{line}</span>
                ))}
                {delivery.would.map((line) => (
                  <span key={line} className="muted">
                    {line}
                  </span>
                ))}
                <div className="w-actions">
                  <button type="button" className="button button-secondary" disabled={busy} onClick={() => void decide({ type: "delivery", delivery: delivery.delivery, decision: "release" })}>
                    {labels.releaseWhole}
                  </button>
                  <button type="button" className="button button-quiet" disabled={busy} onClick={() => void decide({ type: "delivery", delivery: delivery.delivery, decision: "discard" })}>
                    {labels.discardWhole}
                  </button>
                </div>
              </div>
            ))}
            {view.held.batches.length > 0 && (
              <>
                <p className="card-label">{labels.batches}</p>
                <ul className="w-bullets">
                  {view.held.batches.map((batch) => (
                    <li key={batch.batch}>
                      <span className="mono muted">{when(batch.closedAt)}</span> {batch.text}
                    </li>
                  ))}
                </ul>
              </>
            )}
          </section>

          {view.wallet && (
            <section className="card" aria-labelledby="wallet-heading">
              <h2 id="wallet-heading" className="card-heading">
                {labels.wallet}
              </h2>
              <div className="w-money">
                <p>
                  <span className="muted">{labels.balance}</span> <strong>{view.wallet.balance}</strong>
                </p>
                <p>
                  <span className="muted">{labels.spent}</span> <strong>{view.wallet.spent}</strong>{" "}
                  <span className="muted">
                    of {view.wallet.budget}
                  </span>
                </p>
              </div>
              <ul className="w-bars">
                {view.wallet.categories.map((category) => (
                  <li key={category.name}>
                    <div className="w-bar-line">
                      <span>{category.name}</span>
                      <span className="mono muted">
                        {category.spent} / {category.cap}
                      </span>
                    </div>
                    <div className="w-bar" role="meter" aria-valuemin={0} aria-valuemax={100} aria-valuenow={category.percent} aria-label={category.name}>
                      <i style={{ width: `${category.percent}%` }} />
                    </div>
                    {category.paused && <span className="pill pill-failed">{labels.paused}: {category.paused}</span>}
                  </li>
                ))}
              </ul>
              {view.wallet.held.length > 0 && (
                <>
                  <p className="card-label">{labels.heldPayments}</p>
                  {view.wallet.held.map((payment) => (
                    <div key={payment.description} className="decision w-held">
                      <strong>
                        {payment.description} · {payment.amount}
                      </strong>
                      <span>{payment.why}</span>
                    </div>
                  ))}
                </>
              )}
            </section>
          )}

          <section className="card" aria-labelledby="signals-heading">
            <h2 id="signals-heading" className="card-heading">
              {labels.signalsHeading}
            </h2>
            {view.signals.lines.length === 0 ? (
              <p className="card-empty">{labels.noSignals}</p>
            ) : (
              <ul className="w-bullets">
                {view.signals.lines.map((line) => (
                  <li key={`${line.text}-${line.lastAt}`}>
                    {line.text}
                    {line.count > 1 && <span className="muted"> ×{line.count}</span>} <span className="mono muted">{when(line.lastAt)}</span>
                  </li>
                ))}
              </ul>
            )}
          </section>
        </aside>
      </div>
    </main>
  );
}

/** Jobs listed before "Show all": the rest wait behind it. */
const JOBS_SHOWN = 6;

function JobRow({ job, busy, decide }: { job: JobLine; busy: boolean; decide: (command: SiteCommand) => Promise<void> }) {
  const [lowering, setLowering] = useState(false);
  const [reason, setReason] = useState("");
  const id = useId();
  return (
    <li className={`w-job${job.offered ? " w-job-offered" : ""}`}>
      <div className="w-job-line">
        <p className="w-job-title">{job.title}</p>
        <span className="pill pill-neutral">{job.level}</span>
      </div>
      <p className="w-hint">
        {job.area}
        {job.agent ? ` · ${job.agent}` : ""}
        {job.record ? ` · ${job.record}` : ""}
        {job.canLower && !lowering && (
          <>
            {" · "}
            <button type="button" className="link w-small-link" disabled={busy} onClick={() => setLowering(true)}>
              {labels.lower}
            </button>
          </>
        )}
      </p>
      {job.canPromote && !lowering && (
        <p className="row-actions">
          <button type="button" className="button button-primary" disabled={busy} onClick={() => void decide({ type: "promote", job: job.id })}>
            {labels.promote}: {job.next}
          </button>
        </p>
      )}
      {lowering && (
        <form
          className="inline-form"
          onSubmit={(event) => {
            event.preventDefault();
            setLowering(false);
            void decide({ type: "lower", job: job.id, reason });
          }}
        >
          <label htmlFor={id} className="visually-hidden">
            {labels.lowerReason}
          </label>
          <input id={id} placeholder={labels.lowerReason} value={reason} onChange={(e) => setReason(e.target.value)} />
          <button type="submit" className="button button-secondary">
            {labels.lower}
          </button>
        </form>
      )}
    </li>
  );
}

function ReleaseRow({ release }: { release: ReleaseLine }) {
  const [open, setOpen] = useState(false);
  return (
    <li className="w-release">
      <div className="w-release-line">
        <span className="mono w-release-seq">{release.seq}</span>
        <span>{release.source}</span>
        <span className="muted">{when(release.at)}</span>
        <button type="button" className="link" aria-expanded={open} onClick={() => setOpen(!open)}>
          {labels.pagesChanged(release.pages.length)}
        </button>
      </div>
      {open && (
        <ul className="w-release-pages">
          {release.pages.map((page) => (
            <li key={page.path}>
              <Status pill={page.change} /> <span className="mono">{page.path}</span>
            </li>
          ))}
          {release.otherFiles > 0 && <li className="muted">{labels.otherFiles(release.otherFiles)}</li>}
        </ul>
      )}
    </li>
  );
}
