/**
 * A2UI messages as JSON lines (spec 3.3 §6.1 `genui`), in A2UI 1.0 or v0.9.1, ported from
 * adk-ui's `packages/adk-ui-react/src/a2ui/parser.ts` (Apache-2.0): the messages that draw a
 * surface, change its data model and remove it. 1.0 is read into the v0.9.1 shape the store
 * keeps, as the server does (`genui::from_wire`): `{"@path"}` and `{"@call"}` become
 * `{"path"}` and `{"call"}`, `@accessibility` and `@checks` lose their prefix, a literal
 * `"@@x"` key becomes `"@x"`, and a `createSurface` carrying components and a data model sets
 * both. The home itself receives drawings already checked and resolved by the server; a page
 * that renders A2UI from elsewhere reads it with these.
 */
import type { A2uiComponent, SurfaceStore } from "./store";

/** The protocol version written by default. */
export const A2UI_VERSION = "1.0";

/** Every version these messages may carry. */
export const A2UI_VERSIONS = ["1.0", "v0.9.1", "v0.9"] as const;

export type CreateSurfaceMessage = {
  version?: string;
  createSurface: { surfaceId: string; catalogId?: string; components?: A2uiComponent[]; dataModel?: unknown };
};

export type UpdateComponentsMessage = {
  version?: string;
  updateComponents: { surfaceId: string; components: A2uiComponent[] };
};

export type UpdateDataModelMessage = {
  version?: string;
  updateDataModel: { surfaceId: string; path?: string; value?: unknown };
};

export type DeleteSurfaceMessage = {
  version?: string;
  deleteSurface: { surfaceId: string };
};

/** One A2UI agent-to-renderer message that draws, changes or removes a surface. */
export type A2uiMessage = CreateSurfaceMessage | UpdateComponentsMessage | UpdateDataModelMessage | DeleteSurfaceMessage;

/** Each non-empty line of `payload` as a message; a line that is not one throws, naming it. */
export function parseJsonl(payload: string): A2uiMessage[] {
  return payload
    .split("\n")
    .map((line, index) => ({ line: line.trim(), number: index + 1 }))
    .filter(({ line }) => line.length > 0)
    .map(({ line, number }) => {
      const message = JSON.parse(line) as unknown;
      if (!isMessage(message)) throw new Error(`line ${number} is not an A2UI 1.0 or v0.9.1 message`);
      return isV1(message) ? fromV1(message, number) : message;
    });
}

function isMessage(value: unknown): value is A2uiMessage {
  if (typeof value !== "object" || value === null) return false;
  const version = (value as { version?: unknown }).version;
  if (version !== undefined && !A2UI_VERSIONS.includes(version as (typeof A2UI_VERSIONS)[number])) return false;
  const keys = ["createSurface", "updateComponents", "updateDataModel", "deleteSurface"];
  return keys.filter((key) => key in value).length === 1;
}

function isV1(message: A2uiMessage): boolean {
  return message.version === "1.0";
}

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

/** A dynamic value of 1.0 in the v0.9.1 shape. */
function dynamicFromV1(value: Json, line: number): Json {
  if (Array.isArray(value)) return value.map((item) => dynamicFromV1(item, line));
  if (value === null || typeof value !== "object") return value;
  if ("@path" in value) return { path: value["@path"] };
  if ("@call" in value) {
    const out: { [key: string]: Json } = { call: value["@call"] };
    if (value.args !== undefined) out.args = dynamicFromV1(value.args, line);
    return out;
  }
  const out: { [key: string]: Json } = {};
  for (const [key, item] of Object.entries(value)) {
    if (key.startsWith("@@")) out[key.slice(1)] = dynamicFromV1(item, line);
    else if (key.startsWith("@")) throw new Error(`line ${line}: ${key} is a reserved key; write "@${key}" for a literal one`);
    else out[key] = dynamicFromV1(item, line);
  }
  return out;
}

/** A 1.0 component in the v0.9.1 shape. */
function componentFromV1(component: A2uiComponent, line: number): A2uiComponent {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(component)) {
    if (key === "id" || key === "component") out[key] = value;
    else if (key === "@accessibility" || key === "@checks") out[key.slice(1)] = dynamicFromV1(value as Json, line);
    else if (key === "@catalogId" || key === "@metadata") continue;
    else if (key.startsWith("@")) throw new Error(`line ${line}: ${key} is not a component key`);
    else out[key] = dynamicFromV1(value as Json, line);
  }
  return out as A2uiComponent;
}

/** A 1.0 message in the v0.9.1 shape. */
function fromV1(message: A2uiMessage, line: number): A2uiMessage {
  if ("createSurface" in message) {
    const { components, ...rest } = message.createSurface;
    return {
      version: message.version,
      createSurface: { ...rest, ...(components ? { components: components.map((c) => componentFromV1(c, line)) } : {}) },
    };
  }
  if ("updateComponents" in message) {
    return {
      version: message.version,
      updateComponents: {
        surfaceId: message.updateComponents.surfaceId,
        components: message.updateComponents.components.map((c) => componentFromV1(c, line)),
      },
    };
  }
  if ("updateDataModel" in message && !("value" in message.updateDataModel)) {
    throw new Error(`line ${line}: A2UI 1.0 requires value; null removes the path`);
  }
  return message;
}

/** Applies `messages` to `store`, in order. */
export function applyMessages(store: SurfaceStore, messages: A2uiMessage[]): void {
  for (const message of messages) {
    if ("createSurface" in message) {
      const { surfaceId, catalogId, components, dataModel } = message.createSurface;
      store.ensure(surfaceId, catalogId);
      if (dataModel !== undefined) store.updateDataModel(surfaceId, "/", dataModel);
      if (components) store.updateComponents(surfaceId, components);
    } else if ("updateComponents" in message) {
      store.updateComponents(message.updateComponents.surfaceId, message.updateComponents.components);
    } else if ("updateDataModel" in message) {
      store.updateDataModel(message.updateDataModel.surfaceId, message.updateDataModel.path, message.updateDataModel.value);
    } else {
      store.remove(message.deleteSurface.surfaceId);
    }
  }
}
