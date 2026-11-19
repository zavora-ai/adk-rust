import { useEffect, useId, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import type { ElicitationView, ElicitedField, Origin } from "../generated/api";
import { labels } from "../labels";
import { enterNeverDecides, originOf } from "../origin";
import { visible } from "../safe";

type Form = Extract<ElicitationView, { mode: "form" }>;
type Address = Extract<ElicitationView, { mode: "url" }>;

/**
 * What an MCP server asks for a page: its message, then the whole address as text with its
 * host on its own line, so a look-alike stands out, and the reason to look twice when there is
 * one. The home never opens it and never makes it a link (spec 3.3 §7.2).
 */
export function ElicitedAddress({ view }: { view: Address }) {
  return (
    <div className="elicitation">
      <p className="elicitation-message">
        <strong>{visible(view.server)}</strong> · {visible(view.message)}
      </p>
      <p className="elicitation-host">
        {labels.elicitationHost} <strong>{visible(view.host)}</strong>
      </p>
      <code className="elicitation-url">{visible(view.url)}</code>
      {view.warning && (
        <p className="elicitation-warning" role="note">
          {visible(view.warning)}
        </p>
      )}
    </div>
  );
}

/** What a field starts with: its default, or empty. */
function initial(field: ElicitedField): string | boolean {
  if (field.kind === "boolean") return field.default === true;
  if (field.default === undefined || field.default === null) return "";
  return String(field.default);
}

/** The answer the fields hold: empty optional fields left out, numbers as numbers. */
function answerOf(fields: ElicitedField[], values: Record<string, string | boolean>): Record<string, unknown> {
  const answer: Record<string, unknown> = {};
  for (const field of fields) {
    const value = values[field.name];
    if (field.kind === "boolean") {
      answer[field.name] = value === true;
    } else if (typeof value === "string" && value !== "") {
      answer[field.name] = field.kind === "number" || field.kind === "integer" ? Number(value) : value;
    }
  }
  return answer;
}

/** The input type of a field the browser can help with. */
function inputType(field: ElicitedField): string {
  switch (field.kind) {
    case "email":
      return "email";
    case "date":
      return "date";
    case "dateTime":
      return "datetime-local";
    case "number":
    case "integer":
      return "number";
    default:
      return "text";
  }
}

/**
 * "Answer": what an MCP server asks, drawn in the home's own fields from its schema, never by
 * the server. Sending gives the answer as the decision's edit; the engine checks it against the
 * fields before it is on record. Esc closes it; nothing is decided (spec 3.3 §7.2, §8.2).
 */
export function AnswerForm({
  view,
  busy,
  onCancel,
  onSend,
}: {
  view: Form;
  busy: boolean;
  onCancel: () => void;
  onSend: (answer: Record<string, unknown>, origin: Origin) => void;
}) {
  const [values, setValues] = useState<Record<string, string | boolean>>(() =>
    Object.fromEntries(view.fields.map((field) => [field.name, initial(field)])),
  );
  const [missing, setMissing] = useState("");
  const base = useId();
  const first = useRef<HTMLDivElement>(null);
  useEffect(() => first.current?.querySelector<HTMLElement>("input, select")?.focus(), []);

  function escape(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      onCancel();
    }
  }

  function set(name: string, value: string | boolean) {
    setValues((current) => ({ ...current, [name]: value }));
    setMissing("");
  }

  return (
    <div className="edit elicitation" onKeyDown={escape} ref={first}>
      <p className="elicitation-message">
        <strong>{visible(view.server)}</strong> · {visible(view.message)}
      </p>
      {view.fields.map((field) => {
        const id = `${base}-${field.name}`;
        const hint = field.description ? `${id}-hint` : undefined;
        const value = values[field.name];
        const label = (
          <label htmlFor={id} className="edit-label">
            {visible(field.label)}
            {field.required ? "" : ` ${labels.optional}`}
          </label>
        );
        let control;
        if (field.kind === "boolean") {
          control = (
            <input id={id} type="checkbox" checked={value === true} aria-describedby={hint} onChange={(event) => set(field.name, event.target.checked)} />
          );
        } else if (field.kind === "choice") {
          control = (
            <select id={id} value={String(value)} aria-describedby={hint} required={field.required} onChange={(event) => set(field.name, event.target.value)}>
              <option value="">{labels.pickOne}</option>
              {(field.options ?? []).map((option) => (
                <option key={option.value} value={option.value}>
                  {visible(option.label)}
                </option>
              ))}
            </select>
          );
        } else {
          control = (
            <input
              id={id}
              type={inputType(field)}
              value={String(value)}
              aria-describedby={hint}
              required={field.required}
              autoComplete="off"
              spellCheck={false}
              {...(field.minimum != null ? { min: field.minimum } : {})}
              {...(field.maximum != null ? { max: field.maximum } : {})}
              {...(field.kind === "integer" ? { step: 1 } : {})}
              {...(field.minLength != null ? { minLength: field.minLength } : {})}
              {...(field.maxLength != null ? { maxLength: field.maxLength } : {})}
              onChange={(event) => set(field.name, event.target.value)}
            />
          );
        }
        return (
          <div key={field.name} className="elicitation-field">
            {label}
            {control}
            {field.description && (
              <p id={hint} className="edit-hint">
                {visible(field.description)}
              </p>
            )}
          </div>
        );
      })}
      {missing && (
        <p className="decision-problem" role="alert">
          {missing}
        </p>
      )}
      <div className="decision-actions">
        <button
          type="button"
          className="button button-primary"
          disabled={busy}
          onKeyDown={enterNeverDecides}
          onClick={(event) => {
            const absent = view.fields.find(
              (field) => field.required && field.kind !== "boolean" && values[field.name] === "",
            );
            if (absent) {
              setMissing(labels.fillIn(visible(absent.label)));
              return;
            }
            onSend(answerOf(view.fields, values), originOf(event));
          }}
        >
          {labels.sendAnswer}
        </button>
        <button type="button" className="button button-quiet" onClick={onCancel}>
          {labels.cancel}
        </button>
      </div>
    </div>
  );
}
