import type { CallView as Call } from "../generated/api";
import { labels } from "../labels";
import { takeOverOf } from "../takeOver";

/**
 * The exact call, shown in full (spec 3.3 §7.1): the tool and every argument. A call the
 * engine marks truncated is shown cut short until the person expands it, and cannot be
 * approved before. Every value is rendered as text, never as markup (S-I9).
 */
export function CallView({
  call,
  expanded,
  onExpand,
}: {
  call: Call;
  expanded: boolean;
  onExpand: () => void;
}) {
  const ask = takeOverOf(call);
  if (ask) {
    const what = ask.secret === "oneTimeCode" ? labels.aOneTimeCode : ask.secret === "password" ? labels.aPassword : labels.aSecret;
    return (
      <div className="call call-take-over">
        <p>
          <span className="mono">{ask.tool}</span> {labels.asksFor(what)}
        </p>
        {ask.resource && (
          <p>
            {labels.forResource} <span className="mono">{ask.resource}</span>
          </p>
        )}
        <p className="call-note">{labels.secretGoesToTheCall}</p>
      </div>
    );
  }
  if (call.truncated && !expanded) {
    return (
      <div className="call">
        <pre className="call-code" aria-label={labels.exactCall}>
          {call.text}
        </pre>
        <button type="button" className="link" onClick={onExpand}>
          {labels.showWholeCall}
        </button>
      </div>
    );
  }
  const args = call.args;
  const fields =
    args !== null && typeof args === "object" && !Array.isArray(args)
      ? Object.entries(args as Record<string, unknown>)
      : undefined;
  return (
    <div className="call">
      <pre className="call-code" aria-label={labels.exactCall}>
        <span className="code-tool">{call.tool}</span>(
        {fields
          ? fields.map(([key, value]) => (
              <span key={key}>
                {"\n  "}
                {key}: <Value value={value} />,
              </span>
            ))
          : args === null || args === undefined
            ? null
            : <Value value={args} />}
        {fields && fields.length ? "\n" : ""})
      </pre>
      {call.truncated && <p className="call-note">{labels.wholeCallShown}</p>}
    </div>
  );
}

function Value({ value }: { value: unknown }) {
  const text = JSON.stringify(value, null, typeof value === "object" && value !== null ? 2 : undefined) ?? "null";
  if (typeof value === "string") return <span className="code-string">{text}</span>;
  return <span className="code-value">{text.replace(/\n/g, "\n  ")}</span>;
}
