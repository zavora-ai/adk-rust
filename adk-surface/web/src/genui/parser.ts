/**
 * A2UI v0.9.1 messages as JSON lines (spec 3.3 §6.1 `genui`), ported from adk-ui's
 * `packages/adk-ui-react/src/a2ui/parser.ts` (Apache-2.0) without the v1.0 candidate's
 * `actionResponse` and `callFunction`: the four messages that draw a surface, change its data
 * model and remove it. The home itself receives drawings already checked and resolved by the
 * server; a page that renders A2UI from elsewhere reads it with these.
 */
import type { A2uiComponent, SurfaceStore } from "./store";

/** The protocol version the messages carry. */
export const A2UI_VERSION = "v0.9.1";

export type CreateSurfaceMessage = {
  version?: string;
  createSurface: { surfaceId: string; catalogId?: string };
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

/** One A2UI v0.9.1 server message. */
export type A2uiMessage = CreateSurfaceMessage | UpdateComponentsMessage | UpdateDataModelMessage | DeleteSurfaceMessage;

/** Each non-empty line of `payload` as a message; a line that is not one throws, naming it. */
export function parseJsonl(payload: string): A2uiMessage[] {
  return payload
    .split("\n")
    .map((line, index) => ({ line: line.trim(), number: index + 1 }))
    .filter(({ line }) => line.length > 0)
    .map(({ line, number }) => {
      const message = JSON.parse(line) as unknown;
      if (!isMessage(message)) throw new Error(`line ${number} is not an A2UI v0.9.1 message`);
      return message;
    });
}

function isMessage(value: unknown): value is A2uiMessage {
  if (typeof value !== "object" || value === null) return false;
  const keys = ["createSurface", "updateComponents", "updateDataModel", "deleteSurface"];
  return keys.filter((key) => key in value).length === 1;
}

/** Applies `messages` to `store`, in order. */
export function applyMessages(store: SurfaceStore, messages: A2uiMessage[]): void {
  for (const message of messages) {
    if ("createSurface" in message) {
      store.ensure(message.createSurface.surfaceId, message.createSurface.catalogId);
    } else if ("updateComponents" in message) {
      store.updateComponents(message.updateComponents.surfaceId, message.updateComponents.components);
    } else if ("updateDataModel" in message) {
      store.updateDataModel(message.updateDataModel.surfaceId, message.updateDataModel.path, message.updateDataModel.value);
    } else {
      store.remove(message.deleteSurface.surfaceId);
    }
  }
}
