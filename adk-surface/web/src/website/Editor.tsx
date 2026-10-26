import { useEffect, useId, useRef, useState } from "react";
import type { ChangeEvent, FormEvent } from "react";
import type { DraftView, EditorCommand, EditorHome, ItemLine, ScheduleInput } from "../generated/website";
import { problemText } from "../client";
import type { WebsiteClient } from "./client";
import { fileBase64 } from "./client";
import { websiteLabels as labels } from "./labels";
import { Display, Field, Markdown, Status, toLocalInput, valuesOf, when } from "./parts";

/**
 * The web editor (spec 3.5 §7.8.2 "Web editor", §13 Q13): a form per content kind, Markdown
 * text with formatting buttons and a preview, images through the media pipeline, a preview of
 * every page the change touches, scheduling, corrections and withdrawals. No page builder (N4).
 */
export function Editor({ client }: { client: WebsiteClient }) {
  const [home, setHome] = useState<EditorHome>();
  const [draft, setDraft] = useState<DraftView>();
  const [note, setNote] = useState("");
  const [problem, setProblem] = useState("");
  const [busy, setBusy] = useState(false);
  const [kind, setKind] = useState("post");

  useEffect(() => {
    let live = true;
    client
      .editor()
      .then((value) => live && setHome(value))
      .catch((error: unknown) => live && setProblem(problemText(error)));
    return () => {
      live = false;
    };
  }, [client]);

  async function edit(command: EditorCommand) {
    setBusy(true);
    setNote("");
    setProblem("");
    try {
      const outcome = await client.edit(command);
      setHome(outcome.home);
      setDraft(outcome.draft ?? undefined);
      setNote(outcome.note ?? "");
    } catch (error) {
      setProblem(problemText(error));
    } finally {
      setBusy(false);
    }
  }

  async function open(id: string) {
    setProblem("");
    try {
      setDraft(await client.draft(id));
    } catch (error) {
      setProblem(problemText(error));
    }
  }

  return (
    <main className="w-page glow">
      <Display lead={labels.writeLead} accent={labels.writeAccent}>
        {labels.noBuilder}
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
      {home && !home.canEdit && <p className="w-note">{labels.writingNotAllowed}</p>}
      {home?.chain && <p className="always-asks">{home.chain}</p>}
      {home && (
        <div className="w-editor-grid">
          <aside className="w-editor-side">
            {home.canEdit && (
              <form
                className="card w-new"
                onSubmit={(event) => {
                  event.preventDefault();
                  void edit({ type: "new", kind });
                }}
              >
                <label className="card-label" htmlFor="w-new-kind">
                  {labels.newItem}
                </label>
                <select id="w-new-kind" value={kind} onChange={(e) => setKind(e.target.value)}>
                  {home.kinds.map((form) => (
                    <option key={form.kind} value={form.kind}>
                      {form.title}
                    </option>
                  ))}
                </select>
                <p className="w-hint">{home.kinds.find((form) => form.kind === kind)?.description}</p>
                <button type="submit" className="button button-primary" disabled={busy}>
                  {labels.newItem}
                </button>
              </form>
            )}
            <section className="card">
              <p className="card-label">{labels.drafts}</p>
              {home.drafts.length === 0 ? (
                <p className="card-empty">{labels.noDrafts}</p>
              ) : (
                <ul className="w-item-list">
                  {home.drafts.map((line) => (
                    <li key={line.id}>
                      <button type="button" className="w-item" aria-current={draft?.id === line.id ? "true" : undefined} onClick={() => void open(line.id)}>
                        <span>{line.title}</span>
                        <span className="muted">{line.kind}</span>
                        <Status pill={line.status} />
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </section>
            <section className="card">
              <p className="card-label">{labels.content}</p>
              <ul className="w-item-list">
                {home.items.map((item) => (
                  <ItemRow key={item.id} item={item} canEdit={home.canEdit} busy={busy} edit={edit} />
                ))}
              </ul>
            </section>
          </aside>
          <div className="w-editor-main">
            {draft ? (
              <DraftEditor
                key={`${draft.id}-${draft.status.text}-${draft.media.length}`}
                draft={draft}
                home={home}
                busy={busy}
                edit={edit}
                client={client}
                onDraft={setDraft}
                onProblem={setProblem}
              />
            ) : (
              <p className="card muted">{labels.pickSomething}</p>
            )}
          </div>
        </div>
      )}
    </main>
  );
}

function ItemRow({
  item,
  canEdit,
  busy,
  edit,
}: {
  item: ItemLine;
  canEdit: boolean;
  busy: boolean;
  edit: (command: EditorCommand) => Promise<void>;
}) {
  const [mode, setMode] = useState<"correct" | "withdraw">();
  const [text, setText] = useState("");
  const id = useId();
  return (
    <li className="w-item-row">
      <div className="w-item-head">
        <span className="w-item-title">{item.title}</span>
        <Status pill={item.state} />
      </div>
      <p className="w-hint">
        {item.kind} · <span className="mono">{item.slug}</span> · {item.language}
        {item.scheduled ? ` · ${when(item.scheduled)}` : ""}
      </p>
      {canEdit && !mode && (
        <p className="row-actions">
          <button type="button" className="link" disabled={busy} onClick={() => void edit({ type: "open", id: item.id })}>
            {labels.changeItem}
          </button>
          {item.live && (
            <>
              <button type="button" className="link" onClick={() => setMode("correct")}>
                {labels.correct}
              </button>
              <button type="button" className="link" onClick={() => setMode("withdraw")}>
                {labels.withdraw}
              </button>
            </>
          )}
        </p>
      )}
      {mode && (
        <form
          className="w-form"
          onSubmit={(event) => {
            event.preventDefault();
            void edit(mode === "correct" ? { type: "correct", id: item.id, note: text } : { type: "withdraw", id: item.id, notice: text || null });
            setMode(undefined);
            setText("");
          }}
        >
          <label htmlFor={id} className="w-hint">
            {mode === "correct" ? labels.correctionNote : labels.withdrawNotice}
          </label>
          <textarea id={id} rows={2} value={text} onChange={(e) => setText(e.target.value)} />
          <div className="w-actions">
            <button type="submit" className="button button-secondary" disabled={mode === "correct" && !text.trim()}>
              {mode === "correct" ? labels.correct : labels.withdraw}
            </button>
            <button type="button" className="button button-quiet" onClick={() => setMode(undefined)}>
              {labels.cancel}
            </button>
          </div>
        </form>
      )}
    </li>
  );
}

const scheduleFields: Array<[keyof ScheduleInput, string]> = [
  ["publishAt", labels.publishAt],
  ["embargoUntil", labels.embargoUntil],
  ["expiresAt", labels.expiresAt],
  ["revertAt", labels.revertAt],
];

function DraftEditor({
  draft,
  home,
  busy,
  edit,
  client,
  onDraft,
  onProblem,
}: {
  draft: DraftView;
  home: EditorHome;
  busy: boolean;
  edit: (command: EditorCommand) => Promise<void>;
  client: WebsiteClient;
  onDraft: (draft: DraftView) => void;
  onProblem: (problem: string) => void;
}) {
  const [slug, setSlug] = useState(draft.slug);
  const [language, setLanguage] = useState(draft.language || home.languages[0]?.value || "en");
  const [values, setValues] = useState<Record<string, string>>(() => valuesOf(draft.fields));
  const [body, setBody] = useState(draft.body);
  const [schedule, setSchedule] = useState<ScheduleInput>(draft.schedule);
  const [tab, setTab] = useState<"write" | "preview">("write");
  const [alt, setAlt] = useState("");
  const text = useRef<HTMLTextAreaElement>(null);
  const ids = { slug: useId(), language: useId(), body: useId(), file: useId(), alt: useId() };
  const locked = !!draft.approval || draft.status.text === "Published" || draft.status.text === "Scheduled";

  function format(before: string, after = before, line = false) {
    const area = text.current;
    if (!area) return;
    const { selectionStart: start, selectionEnd: end } = area;
    const chosen = body.slice(start, end) || "text";
    const next = line
      ? `${body.slice(0, start)}${before}${chosen}${body.slice(end)}`
      : `${body.slice(0, start)}${before}${chosen}${after}${body.slice(end)}`;
    setBody(next);
    requestAnimationFrame(() => {
      area.focus();
      area.setSelectionRange(start + before.length, start + before.length + chosen.length);
    });
  }

  function save(event?: FormEvent) {
    event?.preventDefault();
    return edit({ type: "save", draft: draft.id, slug, language, fields: values, body, schedule });
  }

  async function upload(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;
    try {
      onDraft(await client.upload(draft.id, { name: file.name, dataBase64: await fileBase64(file), alt: alt || null, generated: false }));
    } catch (error) {
      onProblem(problemText(error));
    }
  }

  const scheduled = !!schedule.publishAt && new Date(schedule.publishAt).getTime() > Date.now();
  return (
    <form className="card w-draft w-form" onSubmit={(event) => void save(event)}>
      <div className="w-section-head">
        <div>
          <h2 className="card-heading">{draft.kindTitle}</h2>
          {draft.changes && <p className="muted">{labels.changesLive}</p>}
        </div>
        <Status pill={draft.status} />
      </div>
      <fieldset className="w-fieldset" disabled={locked}>
        <div className="w-field-row">
          <div className="w-field">
            <label htmlFor={ids.slug}>{labels.slug}</label>
            <input id={ids.slug} className="mono" value={slug} onChange={(e) => setSlug(e.target.value)} aria-describedby={`${ids.slug}-hint`} />
            <p id={`${ids.slug}-hint`} className="w-hint">
              {labels.slugHint}
            </p>
          </div>
          <div className="w-field">
            <label htmlFor={ids.language}>{labels.language}</label>
            <select id={ids.language} value={language} onChange={(e) => setLanguage(e.target.value)}>
              {home.languages.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </div>
        </div>
        {draft.fields.map((field) => (
          <Field key={field.id} spec={field} value={values[field.id] ?? ""} onChange={(value) => setValues((all) => ({ ...all, [field.id]: value }))} />
        ))}
        <div className="w-field">
          <div className="w-body-head">
            <label htmlFor={ids.body}>{labels.body}</label>
            <div className="segmented w-segmented-2" role="tablist">
              <button type="button" role="tab" className="segment" aria-selected={tab === "write"} aria-current={tab === "write" ? "true" : undefined} onClick={() => setTab("write")}>
                {labels.write_}
              </button>
              <button type="button" role="tab" className="segment" aria-selected={tab === "preview"} aria-current={tab === "preview" ? "true" : undefined} onClick={() => setTab("preview")}>
                {labels.preview}
              </button>
            </div>
          </div>
          {tab === "write" ? (
            <>
              <div className="w-toolbar" role="toolbar" aria-label={labels.formatting}>
                <button type="button" className="icon-button" aria-label={labels.bold} title={labels.bold} onClick={() => format("**")}>
                  <strong>B</strong>
                </button>
                <button type="button" className="icon-button" aria-label={labels.italic} title={labels.italic} onClick={() => format("_")}>
                  <em>I</em>
                </button>
                <button type="button" className="icon-button" aria-label={labels.heading} title={labels.heading} onClick={() => format("## ", "", true)}>
                  H
                </button>
                <button type="button" className="icon-button" aria-label={labels.list} title={labels.list} onClick={() => format("- ", "", true)}>
                  •
                </button>
                <button type="button" className="icon-button" aria-label={labels.link} title={labels.link} onClick={() => format("[", "](https://)")}>
                  ↗
                </button>
              </div>
              <textarea id={ids.body} ref={text} rows={10} className="w-body" value={body} onChange={(e) => setBody(e.target.value)} />
            </>
          ) : (
            <div className="w-body-preview">{body.trim() ? <Markdown>{body}</Markdown> : <p className="muted">…</p>}</div>
          )}
        </div>
      </fieldset>

      <section className="w-subsection" aria-label={labels.media}>
        <p className="card-label">{labels.media}</p>
        {draft.media.map((media) => (
          <MediaRow key={media.path} draft={draft.id} media={media} busy={busy || locked} edit={edit} />
        ))}
        {!locked && draft.slug && (
          <div className="w-field-row">
            <div className="w-field">
              <label htmlFor={ids.alt}>{labels.altText}</label>
              <input id={ids.alt} value={alt} onChange={(e) => setAlt(e.target.value)} />
            </div>
            <div className="w-field">
              <label htmlFor={ids.file}>{labels.addImage}</label>
              <input id={ids.file} type="file" accept="image/*" onChange={(event) => void upload(event)} />
            </div>
          </div>
        )}
      </section>

      <fieldset className="w-fieldset w-subsection" disabled={locked}>
        <legend className="card-label">{labels.schedule}</legend>
        <div className="w-field-row w-field-row-4">
          {scheduleFields.map(([key, label]) => (
            <div key={key} className="w-field">
              <label htmlFor={`${draft.id}-${key}`}>{label}</label>
              <input
                id={`${draft.id}-${key}`}
                type="datetime-local"
                value={toLocalInput(schedule[key] ?? "")}
                onChange={(e) => setSchedule((all) => ({ ...all, [key]: e.target.value ? new Date(e.target.value).toISOString() : null }))}
              />
            </div>
          ))}
        </div>
      </fieldset>

      {draft.checkText && (
        <section className="w-subsection" aria-label={labels.checks}>
          <p className={draft.checkText.startsWith("Every") ? "change-note" : "decision-problem"}>{draft.checkText}</p>
          {draft.checks.some((check) => check.findings.length > 0) && (
            <ul className="bordered w-checks">
              {draft.checks
                .filter((check) => check.findings.length > 0)
                .map((check) => (
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
                  </li>
                ))}
            </ul>
          )}
        </section>
      )}

      {draft.preview.length > 0 && (
        <section className="w-subsection" aria-label={labels.changes}>
          <p className="card-label">{labels.changes}</p>
          <ul className="bordered">
            {draft.preview.map((page, index) => (
              <li key={page.path}>
                <div className="w-page-line">
                  <Status pill={page.change} />
                  <span className="w-page-title">{page.title}</span>
                  <span className="mono muted">{page.path}</span>
                </div>
                {index === 0 && page.markdown && <Markdown>{page.markdown}</Markdown>}
              </li>
            ))}
          </ul>
        </section>
      )}

      {draft.approval && <Approvals draft={draft} busy={busy} edit={edit} />}
      {draft.outcome && <p className="change-note">{draft.outcome}</p>}

      {!locked && (
        <div className="w-actions w-draft-actions">
          <button type="submit" className="button button-secondary" disabled={busy}>
            {labels.saveDraft}
          </button>
          <button type="button" className="button button-secondary" disabled={busy || !draft.slug} onClick={() => void edit({ type: "preview", draft: draft.id })}>
            {labels.previewChanges}
          </button>
          <button type="button" className="button button-primary" disabled={busy || !draft.slug} onClick={() => void edit({ type: "publish", draft: draft.id })}>
            {scheduled ? labels.schedulePublish : labels.publish}
          </button>
          <button type="button" className="link" disabled={busy} onClick={() => void edit({ type: "discard", draft: draft.id })}>
            {labels.discardDraft}
          </button>
        </div>
      )}
    </form>
  );
}

function MediaRow({
  draft,
  media,
  busy,
  edit,
}: {
  draft: string;
  media: DraftView["media"][number];
  busy: boolean;
  edit: (command: EditorCommand) => Promise<void>;
}) {
  const [alt, setAlt] = useState(media.alt ?? "");
  const id = useId();
  return (
    <div className="w-media">
      <span className="mono">{media.name}</span>
      {media.generated && <span className="pill pill-neutral">{labels.generated}</span>}
      {media.altConfirmed ? (
        <span className="pill pill-decided">
          {labels.altConfirmed}: {media.alt}
        </span>
      ) : (
        <div className="inline-form">
          <label htmlFor={id} className="visually-hidden">
            {labels.altText}
          </label>
          <input id={id} value={alt} placeholder={labels.altNeeded} onChange={(e) => setAlt(e.target.value)} />
          <button type="button" className="button button-secondary" disabled={busy || !alt.trim()} onClick={() => void edit({ type: "confirmAlt", draft, path: media.path, alt })}>
            {labels.altConfirm}
          </button>
        </div>
      )}
    </div>
  );
}

function Approvals({ draft, busy, edit }: { draft: DraftView; busy: boolean; edit: (command: EditorCommand) => Promise<void> }) {
  const approval = draft.approval!;
  const [reason, setReason] = useState("");
  const [refusing, setRefusing] = useState(false);
  return (
    <section className="decision w-subsection" aria-label={labels.approvals}>
      <p>{approval.text}</p>
      <ul className="w-signer-list">
        {approval.lines.map((line, index) => (
          <li key={`${line.role}-${index}`} className="w-signer">
            <span className="w-signer-role">{line.label}</span>
            <span className="muted">{line.holders.join(", ")}</span>
            <span className={`pill ${line.signedBy ? "pill-decided" : "pill-waiting"}`}>{line.signedBy ?? labels.waitingSignature}</span>
          </li>
        ))}
      </ul>
      <p className="w-hint">{labels.proposedBy(approval.proposer)}</p>
      <div className="w-actions">
        {approval.canApprove.map((role) => (
          <button key={role} type="button" className="button button-primary" disabled={busy} onClick={() => void edit({ type: "approve", draft: draft.id, role })}>
            {labels.approveAs} {role}
          </button>
        ))}
        {approval.canApprove.length > 0 && !refusing && (
          <button type="button" className="button button-secondary" onClick={() => setRefusing(true)}>
            {labels.refuse}
          </button>
        )}
      </div>
      {refusing && (
        <div className="inline-form">
          <input aria-label={labels.refuseReason} placeholder={labels.refuseReason} value={reason} onChange={(e) => setReason(e.target.value)} />
          <button type="button" className="button button-secondary" disabled={!reason.trim()} onClick={() => void edit({ type: "reject", draft: draft.id, reason })}>
            {labels.refuse}
          </button>
        </div>
      )}
    </section>
  );
}
