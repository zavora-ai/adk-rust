/**
 * Links in agent-drawn UI (spec 3.3 §7.4 rule 5), ported from adk-ui's
 * `packages/adk-ui-react/src/security.ts` (Apache-2.0). A drawing never carries a link — the
 * server's validator refuses one anywhere — so a renderer follows none; this is the check a
 * renderer applies to any address before it would, and it is stricter than adk-ui's: plain
 * `http`, loopback included, is never followed, so a drawing cannot reach this computer.
 */

const ABSOLUTE_SCHEME_RE = /^[a-zA-Z][a-zA-Z\d+.-]*:/;

/** What a link would be used for. */
export type SafeUrlKind = "anchor" | "image" | "media";

function isRelativeUrl(value: string): boolean {
  return !ABSOLUTE_SCHEME_RE.test(value) && !value.startsWith("//");
}

/**
 * `raw` when it is safe to follow as `kind`: a path on this server, or `https`; `mailto:` and
 * `tel:` for an anchor. `null` otherwise: `javascript:`, `data:`, `http:` (loopback too) and
 * anything that does not parse.
 */
export function sanitizeUrl(raw: string | null | undefined, kind: SafeUrlKind): string | null {
  const value = raw?.trim();
  if (!value) return null;
  if (isRelativeUrl(value)) return value;
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    return null;
  }
  if (parsed.protocol === "https:") return parsed.toString();
  if (kind === "anchor" && (parsed.protocol === "mailto:" || parsed.protocol === "tel:")) return parsed.toString();
  return null;
}

/** Whether `url` leaves this server. */
export function isExternalNavigationUrl(url: string): boolean {
  return url.startsWith("http://") || url.startsWith("https://");
}
