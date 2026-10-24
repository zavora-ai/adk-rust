/**
 * What this tab remembers across a reload (spec 3.3 §6): which conversation each thread
 * follows and the server's token. Storage can be off, in a private window or with site data
 * blocked; then it lasts as long as the page.
 */
export function readStored<T>(key: string, fallback: T): T {
  try {
    const text = window.sessionStorage.getItem(key);
    return text === null ? fallback : (JSON.parse(text) as T);
  } catch {
    return fallback;
  }
}

export function writeStored(key: string, value: unknown) {
  try {
    if (value === undefined) window.sessionStorage.removeItem(key);
    else window.sessionStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Storage is off: the value lasts for this page only.
  }
}
