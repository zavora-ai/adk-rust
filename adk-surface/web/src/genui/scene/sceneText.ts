import type { DrawnNode, DrawnScene, SceneObject, SceneState } from "../../generated/api";
import { STATE_CRITICAL, STATE_INFO, STATE_OK, STATE_WARNING } from "../../generated/words";
import { visible } from "../../safe";

/** A scene object's state as a word: a state is never only a colour. */
export function stateWord(state: SceneState | null | undefined): string {
  switch (state) {
    case "ok":
      return STATE_OK;
    case "warning":
      return STATE_WARNING;
    case "critical":
      return STATE_CRITICAL;
    case "info":
      return STATE_INFO;
    default:
      return "";
  }
}

/** The CSS token of a state's colour. */
export function stateToken(state: SceneState): string {
  return `--scene-${state}`;
}

/** The states a scene's objects are in, each once, in a fixed order, for its legend. */
export function statesOf(scene: DrawnScene): SceneState[] {
  const present = new Set(scene.objects.map((object) => object.state).filter(Boolean) as SceneState[]);
  return (["ok", "warning", "critical", "info"] as SceneState[]).filter((state) => present.has(state));
}

/** A labelled object of a scene as a line of text. */
export interface LabelledObject {
  id: string;
  label: string;
  state: string;
  value?: string;
}

/** The labelled objects of a scene, in order, their words made safe to show. */
export function labelledObjects(scene: DrawnScene): LabelledObject[] {
  return scene.objects
    .filter((object): object is SceneObject & { label: string } => typeof object.label === "string" && object.label.length > 0)
    .map((object) => ({
      id: object.id,
      label: visible(object.label),
      state: stateWord(object.state),
      value: object.value ?? undefined,
    }));
}

/** The node's child keyed `key`. */
export function childByKey(node: DrawnNode, key: string | null | undefined): DrawnNode | undefined {
  if (!key) return undefined;
  return (node.children ?? []).find((child) => child.key === key);
}

/** The label of the object a panel is shown beside, or its id. */
export function anchorLabel(scene: DrawnScene, anchor: string): string {
  const object = scene.objects.find((candidate) => candidate.id === anchor);
  return visible(object?.label ?? anchor);
}
