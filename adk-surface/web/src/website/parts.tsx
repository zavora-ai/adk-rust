/**
 * Small parts every website view shares: a pill from the desk's tone, a form field from the
 * desk's field spec, label and value lines, and Markdown without raw HTML.
 */
import { useId } from "react";
import type { ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { FieldSpec, Line, Pill as PillView, Tone } from "../generated/website";
import { Pill } from "../components/Pill";
import type { PillKind } from "../components/Pill";

const kinds: Record<Tone, PillKind> = {
  good: "decided",
  waiting: "waiting",
  bad: "failed",
  info: "done",
  neutral: "neutral",
};

/** The desk's pill: a word and its tone, never colour alone (spec 3.3 §9). */
export function Status({ pill }: { pill: PillView }) {
  return <Pill kind={kinds[pill.tone]}>{pill.text}</Pill>;
}

/** Label and value lines, as a definition list. */
export function Lines({ lines, className = "w-lines" }: { lines: Line[]; className?: string }) {
  return (
    <dl className={className}>
      {lines.map((line, index) => (
        <div key={`${line.label}-${index}`} className="w-line">
          <dt>{line.label}</dt>
          <dd>{line.value}</dd>
        </div>
      ))}
    </dl>
  );
}

/** Markdown the site built or a person wrote, with no raw HTML. */
export function Markdown({ children }: { children: string }) {
  return (
    <div className="w-markdown">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        skipHtml
        components={{ a: ({ children: label, href }) => <a href={href} target="_blank" rel="noreferrer">{label}</a> }}
      >
        {children}
      </ReactMarkdown>
    </div>
  );
}

/** A heading with the brand-orange second half, as the web home's displays read. */
export function Display({ lead, accent, children }: { lead: string; accent: string; children?: ReactNode }) {
  return (
    <header className="w-display">
      <h1 className="display">
        {lead} <span className="display-accent">{accent}</span>
      </h1>
      {children && <p className="subhead">{children}</p>}
    </header>
  );
}

/** One form field from the desk's spec. Times travel as ISO strings. */
export function Field({
  spec,
  value,
  onChange,
}: {
  spec: FieldSpec;
  value: string;
  onChange: (value: string) => void;
}) {
  const id = useId();
  const hint = spec.hint ? `${id}-hint` : undefined;
  const common = {
    id,
    name: spec.id,
    required: spec.required,
    "aria-describedby": hint,
  };
  let input: ReactNode;
  switch (spec.kind) {
    case "longText":
    case "markdown":
      input = <textarea {...common} rows={spec.kind === "markdown" ? 8 : 3} value={value} onChange={(e) => onChange(e.target.value)} />;
      break;
    case "choice":
      input = (
        <select {...common} value={value} onChange={(e) => onChange(e.target.value)}>
          <option value="">—</option>
          {(spec.options ?? []).map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>
      );
      break;
    case "dateTime":
      input = (
        <input
          {...common}
          type="datetime-local"
          value={toLocalInput(value)}
          onChange={(e) => onChange(e.target.value ? new Date(e.target.value).toISOString() : "")}
        />
      );
      break;
    default:
      input = (
        <input
          {...common}
          type={inputType(spec.kind)}
          inputMode={spec.kind === "number" ? "decimal" : undefined}
          value={value}
          onChange={(e) => onChange(e.target.value)}
        />
      );
  }
  return (
    <div className="w-field">
      <label htmlFor={id}>
        {spec.label}
        {spec.required && <span className="w-required" aria-hidden="true"> *</span>}
      </label>
      {input}
      {spec.hint && (
        <p id={hint} className="w-hint">
          {spec.hint}
        </p>
      )}
    </div>
  );
}

function inputType(kind: FieldSpec["kind"]): string {
  switch (kind) {
    case "email":
      return "email";
    case "tel":
      return "tel";
    case "url":
      return "url";
    case "time":
      return "time";
    default:
      return "text";
  }
}

/** An ISO time as a `datetime-local` value in this device's time zone. */
export function toLocalInput(iso: string): string {
  if (!iso) return "";
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}T${pad(at.getHours())}:${pad(at.getMinutes())}`;
}

/** The starting values of `fields`. */
export function valuesOf(fields: FieldSpec[]): Record<string, string> {
  return Object.fromEntries(fields.map((field) => [field.id, field.value ?? ""]));
}

/** "25 Oct, 09:14" on this device's clock. */
export function when(iso: string): string {
  const at = new Date(iso);
  return at.toLocaleString("en-GB", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
}
