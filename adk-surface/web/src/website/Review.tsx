import { useCallback, useEffect, useState } from "react";
import type { ReviewCommand, ReviewPage, ReviewView } from "../generated/website";
import { problemText } from "../client";
import type { WebsiteClient } from "./client";
import { websiteLabels as labels } from "./labels";
import { Display, Lines, Markdown, Status } from "./parts";

/**
 * The weekly review (spec 3.5 §7.21): measures against baselines and targets with trends,
 * what each agent did, money, experiments, incidents, leads, freedom earned and lost, alarms
 * that waited, and questions; its one-line summary goes to the owner's channel.
 */
export function Review({
  client,
  onPromote,
  onThreads,
}: {
  client: WebsiteClient;
  /** Accepts a promotion the review offers: the owner's decision on the site view (A-I13). */
  onPromote: (job: string) => Promise<void>;
  onThreads: () => void;
}) {
  const [page, setPage] = useState<ReviewPage>();
  const [problem, setProblem] = useState("");
  const [busy, setBusy] = useState(false);
  const [document, setDocument] = useState(false);

  const load = useCallback(
    (week?: string) =>
      client
        .review(week)
        .then(setPage)
        .catch((error: unknown) => setProblem(problemText(error))),
    [client],
  );
  useEffect(() => {
    void load();
  }, [load]);

  async function decide(command: ReviewCommand) {
    setBusy(true);
    setProblem("");
    try {
      setPage(await client.decideReview(command));
    } catch (error) {
      setProblem(problemText(error));
    } finally {
      setBusy(false);
    }
  }

  async function promote(job: string) {
    setBusy(true);
    setProblem("");
    try {
      await onPromote(job);
      await load(page?.current?.week);
    } catch (error) {
      setProblem(problemText(error));
    } finally {
      setBusy(false);
    }
  }

  const review = page?.current;
  return (
    <main className="w-page glow">
      <Display lead={labels.reviewLead} accent={labels.reviewAccent}>
        {review ? review.title : labels.noReview}
      </Display>
      {problem && (
        <p className="decision-problem" role="alert">
          {problem}
        </p>
      )}
      {page && (
        <div className="w-review-grid">
          <div className="w-site-main">
            {review ? (
              <ReviewBody review={review} busy={busy} onPromote={promote} onThreads={onThreads} document={document} />
            ) : (
              <p className="card">{labels.noReview}</p>
            )}
          </div>
          <aside className="w-site-side">
            {review && (
              <section className="card">
                <p className="card-label">{labels.summary}</p>
                <p className="w-summary">{review.summary}</p>
                {review.sent ? (
                  <p className="change-note">{review.sent}</p>
                ) : (
                  page.channel && (
                    <button type="button" className="button button-primary" disabled={busy} onClick={() => void decide({ type: "send", week: review.week })}>
                      {labels.sendSummary(page.channel)}
                    </button>
                  )
                )}
                <button type="button" className="link w-document-toggle" aria-expanded={document} onClick={() => setDocument(!document)}>
                  {labels.document}
                </button>
              </section>
            )}
            <section className="card">
              <p className="card-label">{labels.pastReviews}</p>
              <ul className="w-bullets">
                {page.reviews.map((line) => (
                  <li key={line.week}>
                    <button type="button" className="link" aria-current={line.week === review?.week ? "true" : undefined} onClick={() => void load(line.week)}>
                      {line.title}
                    </button>
                    {line.sent && <span className="muted"> · {line.sent}</span>}
                  </li>
                ))}
              </ul>
              <p className="muted">
                {labels.nextDue}: {new Date(page.nextDue).toLocaleDateString("en-GB", { day: "numeric", month: "long" })}
              </p>
              {page.canCompose && (
                <button type="button" className="button button-secondary" disabled={busy} onClick={() => void decide({ type: "compose" })}>
                  {labels.compose}
                </button>
              )}
            </section>
          </aside>
        </div>
      )}
    </main>
  );
}

function ReviewBody({
  review,
  busy,
  onPromote,
  onThreads,
  document,
}: {
  review: ReviewView;
  busy: boolean;
  onPromote: (job: string) => Promise<void>;
  onThreads: () => void;
  document: boolean;
}) {
  if (document) {
    return (
      <section className="card">
        <Markdown>{review.markdown}</Markdown>
      </section>
    );
  }
  const list = (title: string, items: string[]) =>
    items.length > 0 && (
      <section className="card">
        <h2 className="card-heading">{title}</h2>
        <ul className="w-bullets">
          {items.map((item) => (
            <li key={item}>{item}</li>
          ))}
        </ul>
      </section>
    );
  return (
    <>
      <div className="stats w-stats">
        {review.headline.map((line) => (
          <div key={line.label} className="stat">
            <span className="stat-label">{line.label}</span>
            <span className="stat-value stat-value-small">{line.value}</span>
          </div>
        ))}
      </div>
      {review.questions.length > 0 && (
        <section className="decision">
          <h2 className="card-heading">{labels.questionsHeading}</h2>
          <ul className="w-bullets">
            {review.questions.map((question) => (
              <li key={question}>{question}</li>
            ))}
          </ul>
          <button type="button" className="link link-accent" onClick={onThreads}>
            {labels.answerInThread}
          </button>
        </section>
      )}
      {(review.promotions.length > 0 || review.demotions.length > 0) && (
        <section className="card">
          <h2 className="card-heading">{labels.autonomyHeading}</h2>
          {review.promotions.map((offer) => (
            <div key={offer.job} className="decision w-held">
              <span className="pill pill-decided">{labels.promotionOffer}</span>
              <strong>{offer.title}</strong>
              <span>
                {offer.from} → {offer.to} · {offer.record}
              </span>
              <div className="w-actions">
                <button type="button" className="button button-primary" disabled={busy} onClick={() => void onPromote(offer.job)}>
                  {labels.promote}: {offer.to}
                </button>
              </div>
            </div>
          ))}
          {review.demotions.length > 0 && <Lines lines={review.demotions} />}
        </section>
      )}
      <section className="card">
        <h2 className="card-heading">{labels.measuresHeading}</h2>
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
            {review.kpis.map((kpi) => (
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
      </section>
      {review.work.length > 0 && (
        <section className="card">
          <h2 className="card-heading">{labels.workHeading}</h2>
          <dl className="w-lines">
            {review.work.map((line) => (
              <div key={line.agent} className="w-line">
                <dt>{line.agent}</dt>
                <dd>
                  <ul className="w-bullets">
                    {line.items.map((item) => (
                      <li key={item}>{item}</li>
                    ))}
                  </ul>
                </dd>
              </div>
            ))}
          </dl>
        </section>
      )}
      {review.money && (
        <section className="card">
          <div className="w-section-head">
            <h2 className="card-heading">{labels.moneyHeading}</h2>
            <Status pill={review.money.within} />
          </div>
          <Lines lines={[{ label: labels.weekSpend, value: review.money.spend }, { label: labels.monthSpend, value: review.money.month }, ...review.money.categories, ...review.money.results]} />
        </section>
      )}
      {review.updates.length > 0 && (
        <section className="card">
          <h2 className="card-heading">{labels.updatesHeading}</h2>
          <Lines lines={review.updates} />
        </section>
      )}
      {list(labels.experimentsHeading, review.experiments)}
      {list(labels.leadsHeading, review.leads)}
      {list(labels.incidentsHeading, review.incidents)}
      {list(labels.alarmsHeading, review.alarms)}
    </>
  );
}
