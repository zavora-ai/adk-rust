/**
 * Surfaces and the values a person types into them (spec 3.3 §6.1 `genui`), ported from
 * adk-ui's `packages/adk-ui-react/src/a2ui/store.ts` (Apache-2.0) without its pending actions:
 * a drawn form's Send is a command the server checks, never an action the renderer tracks.
 */
import type { DrawnNode } from "../generated/api";
import type { Values } from "./bindings";

/** An A2UI component as a message carries it. */
export type A2uiComponent = Record<string, unknown> & { id: string; component: string };

/** One surface: its components by id, its data model and its catalog. */
export interface SurfaceState {
  components: Map<string, A2uiComponent>;
  dataModel: Record<string, unknown>;
  catalogId?: string;
}

/** The value at JSON Pointer `path` in `model`. */
export function getPath(model: unknown, path: string | undefined): unknown {
  if (!path || path === "/") return model;
  let node: unknown = model;
  for (const raw of path.replace(/^\//, "").split("/")) {
    const token = raw.replace(/~1/g, "/").replace(/~0/g, "~");
    if (Array.isArray(node)) node = node[Number(token)];
    else if (typeof node === "object" && node !== null) node = (node as Record<string, unknown>)[token];
    else return undefined;
  }
  return node;
}

/** `model` with `value` at JSON Pointer `path` (removed when `value` is undefined or null), as a new object. */
export function setPath(model: Record<string, unknown>, path: string | undefined, value: unknown): Record<string, unknown> {
  if (!path || path === "/") return (value as Record<string, unknown>) ?? {};
  const tokens = path.replace(/^\//, "").split("/").map((token) => token.replace(/~1/g, "/").replace(/~0/g, "~"));
  const set = (node: unknown, at: number): unknown => {
    const token = tokens[at];
    const container: Record<string, unknown> | unknown[] = Array.isArray(node)
      ? [...node]
      : typeof node === "object" && node !== null
        ? { ...(node as Record<string, unknown>) }
        : {};
    const last = at === tokens.length - 1;
    const next = last ? value : set((container as Record<string, unknown>)[token], at + 1);
    if (Array.isArray(container)) {
      const index = Number(token);
      if (next === undefined || next === null) container.splice(index, 1);
      else container[index] = next;
    } else if (next === undefined || next === null) {
      delete container[token];
    } else {
      container[token] = next;
    }
    return container;
  };
  return set(model, 0) as Record<string, unknown>;
}

/** The surfaces a page renders. */
export class SurfaceStore {
  private surfaces = new Map<string, SurfaceState>();

  get(surfaceId: string): SurfaceState | undefined {
    return this.surfaces.get(surfaceId);
  }

  ensure(surfaceId: string, catalogId?: string): SurfaceState {
    const known = this.surfaces.get(surfaceId);
    if (known) return known;
    const created: SurfaceState = { components: new Map(), dataModel: {}, catalogId };
    this.surfaces.set(surfaceId, created);
    return created;
  }

  updateComponents(surfaceId: string, components: A2uiComponent[]): void {
    const surface = this.ensure(surfaceId);
    for (const component of components) surface.components.set(component.id, component);
  }

  updateDataModel(surfaceId: string, path: string | undefined, value: unknown): void {
    const surface = this.ensure(surfaceId);
    surface.dataModel = setPath(surface.dataModel, path, value);
  }

  remove(surfaceId: string): void {
    this.surfaces.delete(surfaceId);
  }
}

/** Every field of a drawing, in order: `root` and its descendants. */
export function fieldsOf(root: DrawnNode | null | undefined): DrawnNode[] {
  if (!root) return [];
  const out: DrawnNode[] = [];
  const walk = (node: DrawnNode) => {
    if (node.field) out.push(node);
    for (const child of node.children ?? []) walk(child);
  };
  walk(root);
  return out;
}

/** The values a drawing starts with, by data path. */
export function initialValues(root: DrawnNode | null | undefined): Values {
  const values: Values = {};
  for (const node of fieldsOf(root)) {
    if (node.field) values[node.field.path] = node.field.value ?? null;
  }
  return values;
}
