/**
 * Only a person's own click, tap or key takes a screen action, never a script's (spec 3.3 §2
 * item 7): the browser marks the person's input trusted.
 */
import type { SyntheticEvent } from "react";

export function trusted(event: SyntheticEvent): boolean {
  return event.isTrusted;
}
