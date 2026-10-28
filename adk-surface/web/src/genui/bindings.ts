/**
 * The values a person types into a drawn form, and the checks the form makes before it is
 * sent (spec 3.3 §7.4 rule 5). Ported from adk-ui's `packages/adk-ui-react/src/a2ui/bindings.ts`
 * (Apache-2.0), narrowed to what the home renders: every string a person reads arrives
 * resolved by the server, so only data paths and the catalog's check functions are read here
 * — `required`, `length`, `numeric`, `email`, `and`, `or` and `not` — the same ones the server
 * checks again when the form is sent. A check with a `regex` is the server's alone.
 */

/** A form's values, by data path. */
export type Values = Record<string, unknown>;

/** A reference to the value at a data path. */
export interface DataBinding {
  path: string;
}

/** Whether `value` is a binding `{ path }`. */
export function isDataBinding(value: unknown): value is DataBinding {
  return (
    typeof value === "object" &&
    value !== null &&
    !Array.isArray(value) &&
    Object.keys(value).length === 1 &&
    typeof (value as { path?: unknown }).path === "string"
  );
}

/** The value `value` stands for: a binding's value in `values`, or itself. */
export function resolve(value: unknown, values: Values): unknown {
  return isDataBinding(value) ? values[value.path] : value;
}

function text(value: unknown): string {
  if (typeof value === "string") return value;
  if (typeof value === "number") return String(value);
  return "";
}

function bound(args: Record<string, unknown>, name: string): number | undefined {
  const value = args[name];
  return typeof value === "number" ? value : undefined;
}

function truth(value: unknown, values: Values): boolean {
  if (typeof value === "boolean") return value;
  if (typeof value === "object" && value !== null && "call" in value) return holds(value, values);
  return resolve(value, values) === true;
}

/** An email address of one `@`, a part before it, and a domain with a dot inside. */
function isEmail(value: string): boolean {
  const [local, domain, extra] = value.split("@");
  return (
    extra === undefined &&
    !!local &&
    !!domain &&
    domain.includes(".") &&
    !domain.startsWith(".") &&
    !domain.endsWith(".") &&
    !/\s/.test(value)
  );
}

/**
 * Whether the check `condition` holds over `values`. A condition this home cannot read holds:
 * the server refused every other function when the form was drawn.
 */
export function holds(condition: unknown, values: Values): boolean {
  if (typeof condition !== "object" || condition === null || !("call" in condition)) return truth(condition, values);
  const { call, args: rawArgs } = condition as { call: string; args?: unknown };
  const args = (typeof rawArgs === "object" && rawArgs !== null ? rawArgs : {}) as Record<string, unknown>;
  const value = resolve(args.value, values);
  switch (call) {
    case "required":
      if (value === undefined || value === null) return false;
      if (typeof value === "string") return value.trim() !== "";
      if (Array.isArray(value)) return value.length > 0;
      return true;
    case "email": {
      const given = text(value);
      return given === "" || isEmail(given);
    }
    // The server checks a pattern when the form is sent, with an engine whose time is linear
    // in the text: an agent's pattern never runs in the page, where it could backtrack for
    // minutes or read differently.
    case "regex":
      return true;
    case "length": {
      const size = [...text(value)].length;
      const min = bound(args, "min");
      const max = bound(args, "max");
      return (min === undefined || size >= min) && (max === undefined || size <= max);
    }
    case "numeric": {
      if (value === undefined || value === null || (typeof value === "string" && value.trim() === "")) return true;
      const number = typeof value === "number" ? value : Number(String(value).trim());
      if (Number.isNaN(number)) return false;
      const min = bound(args, "min");
      const max = bound(args, "max");
      return (min === undefined || number >= min) && (max === undefined || number <= max);
    }
    case "and":
    case "or": {
      const all = Array.isArray(args.values) ? args.values.map((item) => truth(item, values)) : [];
      return call === "and" ? all.every(Boolean) : all.some(Boolean);
    }
    case "not":
      return !truth(args.value, values);
    default:
      return true;
  }
}

/** Whether `condition` calls `regex` anywhere: the server checks those when the form is sent. */
function checksPattern(condition: unknown): boolean {
  if (Array.isArray(condition)) return condition.some(checksPattern);
  if (typeof condition !== "object" || condition === null) return false;
  return (condition as { call?: unknown }).call === "regex" || Object.values(condition).some(checksPattern);
}

/** The message of the first of `checks` that fails over `values`, if any; a check with a pattern is left to the server. */
export function firstFailure(checks: { condition: unknown; message: string }[], values: Values): string | undefined {
  return checks.find((check) => !checksPattern(check.condition) && !holds(check.condition, values))?.message;
}
