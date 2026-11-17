/**
 * The pure parts of drawing a scene (spec 3.3 §6.1 `genui`, extension catalogs): the home's
 * palette for an object, where the camera goes, how an object moves at a time, and how one
 * version of a scene turns into the next. No WebGL here, so it runs anywhere, tests included.
 */
import type { CameraPreset, DrawnScene, SceneColor, SceneLook, SceneMotion, SceneState } from "../../generated/api";

/** The home's colours by token name, read from its stylesheet. */
export type Palette = Record<string, string>;

/** Every token a scene reads. */
export const SCENE_TOKENS = [
  "--chart-1",
  "--chart-2",
  "--chart-3",
  "--chart-4",
  "--chart-5",
  "--chart-6",
  "--chart-7",
  "--chart-8",
  "--scene-ok",
  "--scene-warning",
  "--scene-critical",
  "--scene-info",
  "--scene-neutral",
  "--scene-muted",
  "--scene-ink",
  "--scene-selected",
  "--scene-ground",
  "--scene-ramp-1",
  "--scene-ramp-2",
  "--scene-ramp-3",
  "--scene-ramp-4",
  "--scene-ramp-5",
  "--chart-axis",
  "--chart-grid",
] as const;

/** Colours to fall back on when a token is not defined. */
const DEFAULTS: Palette = {
  "--chart-1": "#2563eb",
  "--chart-2": "#0d9488",
  "--chart-3": "#7c3aed",
  "--chart-4": "#db2777",
  "--chart-5": "#4d7c0f",
  "--chart-6": "#0e7490",
  "--chart-7": "#475569",
  "--chart-8": "#a21caf",
  "--scene-ok": "#15803d",
  "--scene-warning": "#b45309",
  "--scene-critical": "#b91c1c",
  "--scene-info": "#1d4ed8",
  "--scene-neutral": "#94a3b8",
  "--scene-muted": "#e2e8f0",
  "--scene-ink": "#0f172a",
  "--scene-selected": "#7c3aed",
  "--scene-ground": "#f6f7f9",
  "--scene-ramp-1": "#1e3a8a",
  "--scene-ramp-2": "#0e7490",
  "--scene-ramp-3": "#0d9488",
  "--scene-ramp-4": "#65a30d",
  "--scene-ramp-5": "#ca8a04",
  "--chart-axis": "#64748b",
  "--chart-grid": "#eef0f3",
};

/** The palette as `element` sees it: its tokens, or the defaults. Only `#rrggbb` and `rgb()` pass. */
export function readPalette(element: Element | null): Palette {
  const style = element ? getComputedStyle(element) : undefined;
  const palette: Palette = {};
  for (const token of SCENE_TOKENS) {
    const value = style?.getPropertyValue(token).trim() ?? "";
    palette[token] = /^#[0-9a-f]{6}$/i.test(value) || /^rgba?\([\d\s.,%]+\)$/i.test(value) ? value : DEFAULTS[token];
  }
  return palette;
}

/** The token of a palette colour. */
export function colorToken(color: SceneColor | undefined | null): string {
  switch (color) {
    case "chart2":
      return "--chart-2";
    case "chart3":
      return "--chart-3";
    case "chart4":
      return "--chart-4";
    case "chart5":
      return "--chart-5";
    case "chart6":
      return "--chart-6";
    case "chart7":
      return "--chart-7";
    case "chart8":
      return "--chart-8";
    case "ink":
      return "--scene-ink";
    case "neutral":
      return "--scene-neutral";
    case "muted":
      return "--scene-muted";
    case "ramp":
      return "--scene-ramp-3";
    default:
      return "--chart-1";
  }
}

/** An object's colour: its state's when it has one, else its palette colour. */
export function objectColor(look: SceneLook | undefined, state: SceneState | null | undefined, palette: Palette): string {
  if (state) return palette[`--scene-${state}`] ?? DEFAULTS[`--scene-${state}`];
  return palette[colorToken(look?.color)] ?? DEFAULTS["--chart-1"];
}

/** The chart colour of series `index`. */
export function seriesColor(index: number, palette: Palette): string {
  const token = `--chart-${(Math.max(0, Math.floor(index)) % 8) + 1}`;
  return palette[token] ?? DEFAULTS[token];
}

/** The ramp's colour at `t`, 0 (low) to 1 (high), between its five stops, as `#rrggbb`. */
export function rampColor(t: number, palette: Palette): string {
  const stops = [1, 2, 3, 4, 5].map((n) => hex(palette[`--scene-ramp-${n}`] ?? DEFAULTS[`--scene-ramp-${n}`]));
  const at = Math.min(1, Math.max(0, Number.isFinite(t) ? t : 0)) * (stops.length - 1);
  const low = Math.floor(at);
  const high = Math.min(stops.length - 1, low + 1);
  const mix = at - low;
  const channel = (index: number) => Math.round(stops[low][index] + (stops[high][index] - stops[low][index]) * mix);
  return `#${[0, 1, 2].map((index) => channel(index).toString(16).padStart(2, "0")).join("")}`;
}

function hex(color: string): [number, number, number] {
  const match = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(color);
  if (match) return [parseInt(match[1], 16), parseInt(match[2], 16), parseInt(match[3], 16)];
  const rgb = /^rgba?\(\s*(\d+)[\s,]+(\d+)[\s,]+(\d+)/i.exec(color);
  return rgb ? [Number(rgb[1]), Number(rgb[2]), Number(rgb[3])] : [128, 128, 128];
}

export type Vec3 = [number, number, number];

/** The direction a preset looks from, towards the target. */
export function presetDirection(preset: CameraPreset | undefined): Vec3 {
  switch (preset) {
    case "front":
      return [0, 0.25, 1];
    case "top":
      return [0, 1, 0.001];
    case "side":
      return [1, 0.25, 0];
    case "iso":
      return [1, 0.82, 1];
    default:
      return [0.9, 0.7, 1.15];
  }
}

/** Where the camera goes for a scene whose contents fit a sphere at `centre` of `radius`. */
export function cameraFor(
  scene: Pick<DrawnScene, "camera">,
  centre: Vec3,
  radius: number,
): { position: Vec3; target: Vec3; fov: number } {
  const camera = scene.camera ?? {};
  const fov = Math.min(90, Math.max(15, Number(camera.fov ?? 45)));
  const target = ((camera.target as Vec3 | undefined)?.map(Number) as Vec3 | undefined) ?? centre;
  if (camera.at) return { position: camera.at.map(Number) as Vec3, target, fov };
  const direction = presetDirection(camera.preset);
  const length = Math.hypot(...direction) || 1;
  // A bounding sphere is generous for flat scenes: a little closer than it says.
  const distance = (Math.max(radius, 0.5) / Math.sin((fov * Math.PI) / 360)) * 0.92;
  return {
    position: [0, 1, 2].map((axis) => target[axis] + (direction[axis] / length) * distance) as Vec3,
    target,
    fov,
  };
}

/** How an object stands: where, turned how (radians), scaled how. */
export interface Pose {
  position: Vec3;
  rotation: Vec3;
  scale: Vec3;
}

const AXES = { x: 0, y: 1, z: 2 } as const;

/**
 * The pose of an object that moves, at `t` seconds, from its pose at rest: `null` once a motion
 * that repeats a set number of times has finished, so the object rests.
 */
export function motionAt(motion: SceneMotion, rest: Pose, t: number): Pose | null {
  const seconds = Math.max(0.2, Number(motion.seconds) || 1);
  const delay = Math.max(0, Number(motion.delay ?? 0));
  const time = t - delay;
  if (time < 0) return rest;
  if (motion.repeat != null && time > seconds * motion.repeat) return null;
  const phase = (time / seconds) * Math.PI * 2;
  const axis = AXES[motion.axis ?? "y"] ?? 1;
  const pose: Pose = { position: [...rest.position], rotation: [...rest.rotation], scale: [...rest.scale] };
  switch (motion.kind) {
    case "spin":
      pose.rotation[axis] = rest.rotation[axis] + phase;
      return pose;
    case "bob": {
      const amount = Number(motion.amount ?? 0.25);
      pose.position[axis] = rest.position[axis] + amount * Math.sin(phase);
      return pose;
    }
    case "pulse": {
      const amount = Math.min(1, Number(motion.amount ?? 0.1));
      const grow = 1 + amount * (0.5 + 0.5 * Math.sin(phase));
      pose.scale = rest.scale.map((value) => value * grow) as Vec3;
      return pose;
    }
    case "orbit": {
      // About the parent's centre, on the plane across the axis.
      const [a, b] = axis === 1 ? [0, 2] : axis === 0 ? [1, 2] : [0, 1];
      const radius = Math.hypot(rest.position[a], rest.position[b]);
      const start = Math.atan2(rest.position[b], rest.position[a]);
      pose.position[a] = radius * Math.cos(start + phase);
      pose.position[b] = radius * Math.sin(start + phase);
      return pose;
    }
    case "path": {
      const points = pathPoints(motion.path ?? []);
      if (points.length < 2) return rest;
      const along = ((time / seconds) % 1) * (points.length - 1);
      const index = Math.min(points.length - 2, Math.floor(along));
      const mix = along - index;
      pose.position = [0, 1, 2].map((k) => points[index][k] + (points[index + 1][k] - points[index][k]) * mix) as Vec3;
      return pose;
    }
    default:
      return rest;
  }
}

/** Points from a flat `x, y, z` list. */
export function pathPoints(flat: number[]): Vec3[] {
  const points: Vec3[] = [];
  for (let index = 0; index + 2 < flat.length; index += 3) {
    points.push([Number(flat[index]), Number(flat[index + 1]), Number(flat[index + 2])]);
  }
  return points;
}

/** Whether anything in the scene moves on its own. */
export function moves(scene: DrawnScene): boolean {
  return (scene.motion?.length ?? 0) > 0 || scene.camera?.spin === true;
}

/** `from` eased to `to` at `t`, 0 to 1. */
export function tween(from: number, to: number, t: number): number {
  const eased = t <= 0 ? 0 : t >= 1 ? 1 : 1 - Math.pow(1 - t, 3);
  return from + (to - from) * eased;
}

/** Whether a picked value is one of the field's values. */
export function isPicked(value: string | null | undefined, chosen: unknown): boolean {
  return !!value && Array.isArray(chosen) && chosen.includes(value);
}

/** The field's next values once `value` is picked: one for a choice, toggled for choices. */
export function picking(value: string, chosen: unknown, multiple: boolean): string[] {
  const current = Array.isArray(chosen) ? (chosen.filter((item) => typeof item === "string") as string[]) : [];
  if (!multiple) return [value];
  return current.includes(value) ? current.filter((item) => item !== value) : [...current, value];
}
