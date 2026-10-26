import { useState } from "react";
import type { FormEvent } from "react";
import type { CharterCommand, CharterReview, SectionCard } from "../generated/website";
import { websiteLabels as labels } from "./labels";
import { Field, Lines, Status, valuesOf, when } from "./parts";

/**
 * The charter as decision cards, one per section (spec 3.5 §7.18 stage 5, §8.2): each section
 * looks right or is changed, then every person the approval chain names signs, each a
 * different person (A-I23). Targets wait for a baseline (A-I14).
 */
export function CharterCards({
  charter,
  busy,
  onCommand,
}: {
  charter: CharterReview;
  busy: boolean;
  onCommand: (command: CharterCommand) => void;
}) {
  const committed = charter.committed;
  return (
    <div className="w-charter">
      <div className="w-charter-head">
        <div>
          <h2 className="card-heading">
            {labels.charterHeading} · version {charter.version}
          </h2>
          <p className="muted">{charter.kind}</p>
        </div>
        <Status pill={charter.status} />
      </div>
      {!committed && <p className="subhead w-charter-sub">{labels.charterSubhead}</p>}
      <section className="w-signers decision" aria-label={labels.signers}>
        <p className="card-label">
          {labels.signers} · {charter.reviewed} of {charter.sections.length} sections read
        </p>
        <ul className="w-signer-list">
          {charter.signers.map((line, index) => (
            <li key={`${line.role}-${index}`} className="w-signer">
              <span className="w-signer-role">{line.label}</span>
              <span className="muted">{line.holders.join(", ") || labels.nobodyHolds}</span>
              {line.signedBy ? (
                <span className="pill pill-decided">
                  {labels.signed} · {line.signedBy}
                  {line.signedAt ? `, ${when(line.signedAt)}` : ""}
                </span>
              ) : (
                <span className="pill pill-waiting">{labels.waitingSignature}</span>
              )}
            </li>
          ))}
        </ul>
        {charter.problems.length > 0 && (
          <ul className="w-problems">
            {charter.problems.map((problem) => (
              <li key={problem}>{problem}</li>
            ))}
          </ul>
        )}
        {committed ? (
          <p className="change-note" role="status">
            {labels.inForce(committed.version, when(committed.at), committed.signedBy.join(" and "))}
          </p>
        ) : (
          <div className="w-actions">
            {charter.canSign.map((role) => (
              <button
                key={role}
                type="button"
                className="button button-primary"
                disabled={busy}
                onClick={() => onCommand({ type: "sign", role })}
              >
                {labels.sign} {charter.signers.find((line) => line.role === role)?.label.toLowerCase() ?? role}
              </button>
            ))}
          </div>
        )}
      </section>
      <div className="w-section-grid">
        {charter.sections.map((section) => (
          <Section key={section.id} section={section} busy={busy || !!committed} onCommand={onCommand} />
        ))}
      </div>
    </div>
  );
}

function Section({
  section,
  busy,
  onCommand,
}: {
  section: SectionCard;
  busy: boolean;
  onCommand: (command: CharterCommand) => void;
}) {
  const [editing, setEditing] = useState(false);
  const [values, setValues] = useState<Record<string, string>>(() => valuesOf(section.fields));

  function save(event: FormEvent) {
    event.preventDefault();
    onCommand({ type: "edit", section: section.id, values });
    setEditing(false);
  }

  return (
    <article className={`card w-section${section.reviewed ? "" : " w-section-open"}`} aria-labelledby={`section-${section.id}`}>
      <div className="w-section-head">
        <h3 id={`section-${section.id}`} className="w-section-title">
          {section.title}
        </h3>
        {section.reviewed && <span className="pill pill-decided">{labels.read}</span>}
      </div>
      <p className="muted w-section-summary">{section.summary}</p>
      {editing ? (
        <form className="w-form" onSubmit={save}>
          {section.fields.map((field) => (
            <Field
              key={field.id}
              spec={field}
              value={values[field.id] ?? ""}
              onChange={(value) => setValues((all) => ({ ...all, [field.id]: value }))}
            />
          ))}
          <div className="w-actions">
            <button type="submit" className="button button-primary" disabled={busy}>
              {labels.save}
            </button>
            <button type="button" className="button button-quiet" onClick={() => setEditing(false)}>
              {labels.cancel}
            </button>
          </div>
        </form>
      ) : (
        <>
          <Lines lines={section.lines} />
          {section.note && <p className="w-note">{section.note}</p>}
          {!busy && (
            <div className="w-actions">
              {!section.reviewed && (
                <button
                  type="button"
                  className="button button-secondary"
                  onClick={() => onCommand({ type: "review", section: section.id })}
                >
                  {labels.looksRight}
                </button>
              )}
              {section.fields.length > 0 && (
                <button
                  type="button"
                  className="link"
                  onClick={() => {
                    setValues(valuesOf(section.fields));
                    setEditing(true);
                  }}
                >
                  {labels.changeIt}
                </button>
              )}
            </div>
          )}
        </>
      )}
    </article>
  );
}
