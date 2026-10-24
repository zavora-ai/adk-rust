/**
 * How an input was given (spec 3.3 §6, §11): every command carries it, and it reaches the
 * record, so "What your agents did" can say how a person answered.
 */
import type { KeyboardEvent, MouseEvent } from "react";
import type { Origin } from "./generated/api";

/** The origin of a click: the keyboard when a key pressed the button, touch or the pointer. */
export function originOf(event: MouseEvent | KeyboardEvent): Origin {
  if ("key" in event) return { kind: "keyboard" };
  if (event.detail === 0) return { kind: "keyboard" };
  const pointer = (event.nativeEvent as PointerEvent | undefined)?.pointerType;
  return pointer === "touch" ? { kind: "touch" } : { kind: "pointer" };
}

/**
 * Enter never decides (spec 3.3 §7.4): a decision's buttons answer to a click, a tap or the
 * space bar, never to the Enter that sends a message.
 */
export function enterNeverDecides(event: KeyboardEvent<HTMLButtonElement>) {
  if (event.key === "Enter") event.preventDefault();
}
