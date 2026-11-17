/**
 * What the page remembers of a scene between its versions (spec 3.3 §6.1 `genui`): where each
 * object stood and where the person left the camera. When `patch_surface` draws the next
 * version, objects that keep their ids move to their new places and the camera stays put.
 */
import type { Vec3 } from "./plan";

/** One object as it was last drawn: its place and, for bars, their heights. */
export interface Remembered {
  position: Vec3;
  scale: Vec3;
  heights?: number[];
}

export interface SceneMemory {
  version: number;
  objects: Map<string, Remembered>;
  camera?: { position: Vec3; target: Vec3 };
}

const memory = new Map<string, SceneMemory>();
/** Forget the oldest beyond this many scenes. */
const KEEP = 32;

/** What is remembered of the scene `key` (its surface and node). */
export function recall(key: string): SceneMemory | undefined {
  return memory.get(key);
}

/** Remembers the scene `key` at `version`. */
export function remember(key: string, value: SceneMemory): void {
  memory.delete(key);
  memory.set(key, value);
  while (memory.size > KEEP) {
    const oldest = memory.keys().next().value;
    if (oldest === undefined) break;
    memory.delete(oldest);
  }
}
