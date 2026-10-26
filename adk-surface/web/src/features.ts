/**
 * Features a server may be built without (spec 3.3 §6.1). The committed page is the same for
 * every Rust feature set, so the home asks at run time: each feature's part of the page reads
 * its own route through `probe`, and stays hidden while the route answers 404, 405 or 501 —
 * a server built without that feature. Any other failure is an error, shown as one.
 */
import { ApiError } from "./client";

/** The statuses of a route a server was built without. */
export const ABSENT_STATUSES: readonly number[] = [404, 405, 501];

/** Whether `error` says the server serves no such route. */
export function isAbsent(error: unknown): boolean {
  return error instanceof ApiError && ABSENT_STATUSES.includes(error.status);
}

/**
 * What `read` reads from a feature's route, or `undefined` when the server was built without
 * the feature. One line per feature, such as the website's tab:
 *
 * ```ts
 * const site = await probe(() => website.home());
 * ```
 */
export async function probe<T>(read: () => Promise<T>): Promise<T | undefined> {
  try {
    return await read();
  } catch (error) {
    if (isAbsent(error)) return undefined;
    throw error;
  }
}
