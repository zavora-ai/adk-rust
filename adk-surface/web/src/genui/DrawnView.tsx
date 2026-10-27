import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import type { DrawnActionView, DrawnField, DrawnNode, DrawnSurface, FormState } from "../generated/api";
import { FORM_CHANGED_SEND_AGAIN, NOT_SENT_YET, SENDING } from "../generated/words";
import { ApiError } from "../client";
import { Avatar } from "../components/Avatar";
import { visible } from "../safe";
import type { Values } from "./bindings";
import { firstFailure } from "./bindings";
import { fieldsOf, initialValues } from "./store";
import "./genui.css";

/** How many rows of a drawn table show before "Show all". */
export const SHOWN_ROWS = 10;

/** What sending a form asks of the page: the action and the values, by data path. */
export type SendForm = (actionId: string, values: Values) => Promise<void>;

/**
 * Something an agent drew (spec 3.3 §6.1 `genui`, §7.4 rule 5): a form, card, table or screen
 * on the normal card background — `--card` and `--border`, never a decision's colours, no
 * status pills — under "Drawn by Finance · 10:02". Every string arrives resolved by the server
 * and is drawn as text; the page binds only the values the person types. A form's button
 * carries the engine's line "Sending runs send_invoice and asks you first"; what happened
 * once it was sent comes from `state`, the decision and the call's result, never from the
 * click.
 */
export function DrawnView({
  surface,
  superseded = false,
  changed = false,
  state,
  onSend,
  below,
  sent,
}: {
  surface: DrawnSurface;
  /** A newer version replaces this one: it is drawn collapsed under "Earlier version · 10:02". */
  superseded?: boolean;
  /** An earlier version is in the thread: "Finance changed this form at 10:05" goes above it. */
  changed?: boolean;
  /** The form's state once sent. */
  state?: FormState;
  /** Sends the form; absent where it cannot be sent from here. */
  onSend?: SendForm;
  /** What follows the form: the decision its Send asked for. */
  below?: ReactNode;
  /** The call its Send asked for, as the decision shows it: the values the person sent. */
  sent?: { actionId: string; args: Record<string, unknown> };
}) {
  if (surface.deleted) {
    return <p className="drawn-changed">{visible(surface.changedText)}</p>;
  }
  if (superseded) {
    return (
      <details className="drawn-earlier">
        <summary>{visible(surface.earlierText)}</summary>
        <DrawnCard surface={surface} state={state} readOnly />
      </details>
    );
  }
  return (
    <div className="drawn-place">
      {changed && <p className="drawn-changed">{visible(surface.changedText)}</p>}
      <DrawnCard surface={surface} state={state} onSend={onSend} sent={sent} />
      {below}
    </div>
  );
}

function DrawnCard({
  surface,
  state,
  onSend,
  sent,
  readOnly = false,
}: {
  surface: DrawnSurface;
  state?: FormState;
  onSend?: SendForm;
  sent?: { actionId: string; args: Record<string, unknown> };
  readOnly?: boolean;
}) {
  const [typedHere, setValues] = useState<Values>(() => initialValues(surface.root));
  const [touched, setTouched] = useState(false);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [sending, setSending] = useState(false);
  const [problem, setProblem] = useState<string>();
  // The server's word on the send ends the wait, whatever it says.
  useEffect(() => {
    if (state) setSending(false);
  }, [state]);
  const locked = readOnly || !!state?.locked;
  const actions = surface.actions ?? [];
  // A form sent shows what was sent, read back from its call.
  const values = locked && sent ? sentValues(surface, sent, typedHere) : typedHere;

  const change = (path: string, value: unknown) => {
    setValues((all) => ({ ...all, [path]: value }));
    setTouched(true);
    setProblem(undefined);
    setErrors((all) => {
      if (!(path in all)) return all;
      const { [path]: _gone, ...rest } = all;
      return rest;
    });
  };

  const send = async (action: string) => {
    if (!onSend || locked || sending) return;
    const found: Record<string, string> = {};
    const typed: Values = {};
    for (const node of fieldsOf(surface.root)) {
      const field = node.field as DrawnField;
      const { value, problem: wrong } = typedValue(field, values[field.path]);
      if (wrong) found[field.path] = wrong;
      typed[field.path] = value;
    }
    for (const node of fieldsOf(surface.root)) {
      const field = node.field as DrawnField;
      const failure = firstFailure(field.checks ?? [], typed);
      if (failure && !found[field.path]) found[field.path] = failure;
    }
    setErrors(found);
    if (Object.keys(found).length > 0) return;
    setSending(true);
    setProblem(undefined);
    try {
      await onSend(action, typed);
    } catch (error) {
      setSending(false);
      setProblem(
        error instanceof ApiError && error.code === "surface.drawn_changed"
          ? FORM_CHANGED_SEND_AGAIN
          : error instanceof Error
            ? error.message
            : String(error),
      );
    }
  };

  const refused = state?.status === "refused";
  const failing = Object.keys(errors).length > 0;
  const inline = refused ? state?.text : touched && !sending && !failing && !problem ? NOT_SENT_YET : undefined;
  const context: RenderContext = { values, errors, locked, sending, change, send, surface, inline };
  return (
    <section className="drawn" aria-label={surface.title ?? surface.drawnText}>
      <header className="drawn-by">
        <Avatar agent={surface.agent} size="sm" />
        <span>{visible(surface.drawnText)}</span>
      </header>
      {surface.title && <h3 className="drawn-title">{visible(surface.title)}</h3>}
      {surface.root && <div className="drawn-body">{render(surface.root, context, true)}</div>}
      {state && !refused ? (
        <p className="drawn-status" role="status">
          {visible(state.text)}
        </p>
      ) : (
        !locked &&
        actions.length > 0 && (
          <div className="drawn-foot">
            {problem && (
              <p className="drawn-problem" role="alert">
                {visible(problem)}
              </p>
            )}
            {actions.map((action) => (
              <Note key={action.id} action={action} />
            ))}
          </div>
        )
      )}
    </section>
  );
}

/** `typed` with each field the sent action bound replaced by the value the call carries. */
function sentValues(
  surface: DrawnSurface,
  sent: { actionId: string; args: Record<string, unknown> },
  typed: Values,
): Values {
  const action = (surface.actions ?? []).find((known) => known.id === sent.actionId);
  const values = { ...typed };
  for (const arg of action?.args ?? []) {
    if (arg.path && arg.name in sent.args) values[arg.path] = sent.args[arg.name];
  }
  return values;
}

/** "Sending runs `send_invoice` and asks you first", with the tool in mono. */
function Note({ action }: { action: DrawnActionView }) {
  const [before, ...rest] = action.note.split(action.tool);
  return (
    <p className="drawn-note">
      {visible(before)}
      {rest.length > 0 && <code>{visible(action.tool)}</code>}
      {visible(rest.join(action.tool))}
    </p>
  );
}

/** A value as the server takes it, or what is wrong with it. */
function typedValue(field: DrawnField, value: unknown): { value: unknown; problem?: string } {
  switch (field.input) {
    case "number":
    case "slider": {
      if (value === "" || value === undefined || value === null) return { value: null };
      const number = typeof value === "number" ? value : Number(String(value).trim());
      return Number.isNaN(number) ? { value, problem: "Enter a number" } : { value: number };
    }
    case "checkbox":
      return { value: value === true };
    case "choice":
    case "choices":
      return { value: Array.isArray(value) ? value : value ? [value] : [] };
    default:
      return { value: typeof value === "string" ? value : value == null ? "" : String(value) };
  }
}

interface RenderContext {
  values: Values;
  errors: Record<string, string>;
  locked: boolean;
  sending: boolean;
  change: (path: string, value: unknown) => void;
  send: (action: string) => void;
  surface: DrawnSurface;
  /** Beside the buttons: "Not sent yet", or why the agent cannot use the tool. */
  inline?: string;
}

/** Numbers, dates and codes such as `[CLIENT]` read in mono (spec 3.3 §9). */
function codeLike(text: string | null | undefined): boolean {
  return !!text && /^[\d\s.,:/+\-[\]A-Z_]+$/.test(text) && /[\d[A-Z]/.test(text);
}

function textOf(node: DrawnNode): string {
  return visible(node.text ?? "");
}

/** A Column of a caption Row then a List of Rows: a table. */
function asTable(nodes: DrawnNode[], at: number): { header: DrawnNode; list: DrawnNode } | undefined {
  const header = nodes[at];
  const list = nodes[at + 1];
  const captions = (node: DrawnNode) =>
    node.component === "Row" &&
    (node.children ?? []).length > 0 &&
    (node.children ?? []).every((child) => child.component === "Text" && child.variant === "caption");
  if (!header || !list || !captions(header) || list.component !== "List") return undefined;
  const rows = list.children ?? [];
  return rows.every((row) => row.component === "Row") ? { header, list } : undefined;
}

function render(node: DrawnNode, context: RenderContext, root = false): ReactNode {
  switch (node.component) {
    case "Text":
      return <DrawnText key={node.key} node={node} />;
    case "Card":
      return root ? (
        <div key={node.key} className="drawn-stack">
          {(node.children ?? []).map((child) => render(child, context))}
        </div>
      ) : (
        <div key={node.key} className="drawn-inner-card">
          {(node.children ?? []).map((child) => render(child, context))}
        </div>
      );
    case "Column":
      return <Column key={node.key} node={node} context={context} />;
    case "Row":
      return <Row key={node.key} node={node} context={context} />;
    case "List":
      return <ListNode key={node.key} node={node} context={context} />;
    case "Divider":
      return <hr key={node.key} className="drawn-divider" />;
    case "Tabs":
      return <TabsNode key={node.key} node={node} context={context} />;
    case "Button":
      return <ButtonNode key={node.key} node={node} context={context} />;
    default:
      return node.field ? <Field key={node.key} node={node} context={context} /> : null;
  }
}

function DrawnText({ node }: { node: DrawnNode }) {
  switch (node.variant) {
    case "h1":
    case "h2":
    case "h3":
    case "h4":
    case "h5":
      return <p className={`drawn-heading drawn-${node.variant}`}>{textOf(node)}</p>;
    case "caption":
      return <span className="drawn-caption">{textOf(node)}</span>;
    default:
      return <p className="drawn-text">{textOf(node)}</p>;
  }
}

function Column({ node, context }: { node: DrawnNode; context: RenderContext }) {
  const children = node.children ?? [];
  const out: ReactNode[] = [];
  for (let at = 0; at < children.length; at += 1) {
    const table = asTable(children, at);
    if (table) {
      out.push(<Table key={table.list.key} header={table.header} list={table.list} />);
      at += 1;
    } else {
      out.push(render(children[at], context));
    }
  }
  return <div className="drawn-column">{out}</div>;
}

function Row({ node, context }: { node: DrawnNode; context: RenderContext }) {
  const children = node.children ?? [];
  // A card's row: a caption label and its value.
  if (children.length === 2 && children.every((child) => child.component === "Text") && children[0].variant === "caption") {
    return (
      <div className="drawn-pair">
        <span className="drawn-label">{textOf(children[0])}</span>
        <span className={codeLike(children[1].text) ? "drawn-value drawn-mono" : "drawn-value"}>{textOf(children[1])}</span>
      </div>
    );
  }
  const buttons = children.length > 0 && children.every((child) => child.component === "Button");
  if (buttons && context.locked) return null;
  return (
    <div className={buttons ? "drawn-actions" : "drawn-row"} data-justify={node.variant ?? undefined}>
      {children.map((child) => render(child, context))}
      {buttons && context.inline && (
        <span className="drawn-status-inline" role="status">
          {visible(context.inline)}
        </span>
      )}
    </div>
  );
}

function ListNode({ node, context }: { node: DrawnNode; context: RenderContext }) {
  const [all, setAll] = useState(false);
  const children = node.children ?? [];
  const shown = all ? children : children.slice(0, SHOWN_ROWS);
  return (
    <div className="drawn-list" data-direction={node.variant ?? undefined}>
      {shown.map((child) => render(child, context))}
      {!all && children.length > SHOWN_ROWS && node.showAllText && (
        <button type="button" className="drawn-show-all" onClick={() => setAll(true)}>
          {visible(node.showAllText)}
        </button>
      )}
    </div>
  );
}

function Table({ header, list }: { header: DrawnNode; list: DrawnNode }) {
  const [all, setAll] = useState(false);
  const rows = list.children ?? [];
  const shown = all ? rows : rows.slice(0, SHOWN_ROWS);
  return (
    <div className="drawn-table-wrap">
      <table className="drawn-table">
        <thead>
          <tr>
            {(header.children ?? []).map((cell) => (
              <th key={cell.key} scope="col">
                {textOf(cell)}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {shown.map((row) => (
            <tr key={row.key}>
              {(row.children ?? []).map((cell) => (
                <td key={cell.key} className={codeLike(cell.text) ? "drawn-mono" : undefined}>
                  {textOf(cell)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      {!all && rows.length > SHOWN_ROWS && list.showAllText && (
        <button type="button" className="drawn-show-all" onClick={() => setAll(true)}>
          {visible(list.showAllText)}
        </button>
      )}
    </div>
  );
}

function TabsNode({ node, context }: { node: DrawnNode; context: RenderContext }) {
  const [selected, setSelected] = useState(0);
  const tabs = node.tabs ?? [];
  const children = node.children ?? [];
  return (
    <div className="drawn-tabs">
      <div role="tablist">
        {tabs.map((title, index) => (
          <button
            key={index}
            type="button"
            role="tab"
            aria-selected={index === selected}
            className={index === selected ? "drawn-tab drawn-tab-selected" : "drawn-tab"}
            onClick={() => setSelected(index)}
          >
            {visible(title)}
          </button>
        ))}
      </div>
      <div role="tabpanel">{children[selected] && render(children[selected], context)}</div>
    </div>
  );
}

function ButtonNode({ node, context }: { node: DrawnNode; context: RenderContext }) {
  if (context.locked) return null;
  const primary = node.variant === "primary";
  const action = node.action;
  return (
    <button
      type="button"
      className={primary ? "button button-stop drawn-send" : "button button-secondary drawn-send"}
      disabled={context.sending || !action}
      aria-busy={context.sending || undefined}
      onClick={() => action && context.send(action)}
    >
      {context.sending ? SENDING : textOf(node)}
    </button>
  );
}

function Field({ node, context }: { node: DrawnNode; context: RenderContext }) {
  const field = node.field as DrawnField;
  const id = `drawn-${context.surface.surfaceId}-${context.surface.version}-${node.key}`.replace(/[^\w-]/g, "_");
  const value = context.values[field.path];
  const error = context.errors[field.path];
  const disabled = context.locked || context.sending;
  const label = textOf(node);
  const set = (next: unknown) => context.change(field.path, next);
  const described = error ? `${id}-error` : undefined;
  let input: ReactNode;
  switch (field.input) {
    case "checkbox":
      return (
        <label className="drawn-check">
          <input type="checkbox" checked={value === true} disabled={disabled} onChange={(event) => set(event.target.checked)} />
          <span>{label}</span>
        </label>
      );
    case "choice":
    case "choices": {
      const chosen = Array.isArray(value) ? (value as string[]) : [];
      return (
        <fieldset className="drawn-field drawn-choices" disabled={disabled}>
          <legend className="drawn-field-label">{label}</legend>
          {(field.options ?? []).map((option) => (
            <label key={option.value} className="drawn-check">
              <input
                type={field.input === "choice" ? "radio" : "checkbox"}
                name={id}
                checked={chosen.includes(option.value)}
                onChange={(event) =>
                  set(
                    field.input === "choice"
                      ? [option.value]
                      : event.target.checked
                        ? [...chosen, option.value]
                        : chosen.filter((known) => known !== option.value),
                  )
                }
              />
              <span>{visible(option.label)}</span>
            </label>
          ))}
          {error && (
            <span id={described} className="drawn-error">
              {visible(error)}
            </span>
          )}
        </fieldset>
      );
    }
    case "longText":
      input = (
        <textarea
          id={id}
          className="drawn-input"
          value={stringOf(value)}
          disabled={disabled}
          aria-invalid={!!error || undefined}
          aria-describedby={described}
          onChange={(event) => set(event.target.value)}
        />
      );
      break;
    case "slider":
      input = (
        <input
          id={id}
          type="range"
          className="drawn-range"
          min={numberOf(field.min) ?? 0}
          max={numberOf(field.max) ?? 100}
          value={numberOf(value) ?? numberOf(field.min) ?? 0}
          disabled={disabled}
          onChange={(event) => set(Number(event.target.value))}
        />
      );
      break;
    default:
      input = (
        <input
          id={id}
          type={inputType(field)}
          inputMode={field.input === "number" ? "decimal" : undefined}
          className="drawn-input"
          value={stringOf(value)}
          disabled={disabled}
          aria-invalid={!!error || undefined}
          aria-describedby={described}
          onChange={(event) => set(event.target.value)}
        />
      );
  }
  return (
    <div className="drawn-field">
      <label htmlFor={id} className="drawn-field-label">
        {label}
      </label>
      {input}
      {error && (
        <span id={described} className="drawn-error">
          {visible(error)}
        </span>
      )}
    </div>
  );
}

function inputType(field: DrawnField): string {
  switch (field.input) {
    case "date":
      return "date";
    case "time":
      return "time";
    case "dateTime":
      return "datetime-local";
    default:
      return "text";
  }
}

function stringOf(value: unknown): string {
  if (value === undefined || value === null) return "";
  return typeof value === "string" ? value : String(value);
}

function numberOf(value: unknown): number | undefined {
  if (typeof value === "number") return value;
  if (typeof value === "string" && value.trim() !== "" && !Number.isNaN(Number(value))) return Number(value);
  return undefined;
}
