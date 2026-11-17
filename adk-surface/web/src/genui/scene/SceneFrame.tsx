import type { ReactNode } from "react";
import type { DrawnNode, DrawnScene } from "../../generated/api";
import { visible } from "../../safe";
import { anchorLabel, childByKey, labelledObjects, stateToken, stateWord, statesOf } from "./sceneText";

/** What a scene's frame needs of the drawing it is in. */
export interface SceneFrameProps {
  /** The scene's node: its children are its panels, then its text view. */
  node: DrawnNode;
  scene: DrawnScene;
  /** Draws a held node as the home draws it. */
  renderChild: (node: DrawnNode) => ReactNode;
}

/**
 * A 3D scene an agent drew (spec 3.3 §6.1 `genui`, extension catalogs): the server's sentence
 * for it as a visible caption, the legend of its states (a word and a colour), what picking
 * fills, and the scene. This build shows a scene in words: its text view or its labelled
 * objects, then each panel under the object it is shown beside.
 */
export function SceneFrame({ node, scene, renderChild }: SceneFrameProps) {
  const states = statesOf(scene);
  return (
    <figure className="drawn-scene" data-scene="text">
      <figcaption className="drawn-scene-summary">{visible(scene.summary)}</figcaption>
      {states.length > 0 && (
        <ul className="drawn-scene-legend">
          {states.map((state) => (
            <li key={state}>
              <span className="drawn-scene-swatch" style={{ background: `var(${stateToken(state)})` }} aria-hidden="true" />
              {stateWord(state)}
            </li>
          ))}
        </ul>
      )}
      {scene.select?.note && <p className="drawn-scene-note">{visible(scene.select.note)}</p>}
      <SceneText node={node} scene={scene} renderChild={renderChild} />
    </figure>
  );
}

/** A scene in words: its text view, or its labelled objects; then its panels. */
export function SceneText({ node, scene, renderChild }: SceneFrameProps) {
  const words = childByKey(node, scene.textView);
  const objects = labelledObjects(scene);
  return (
    <div className="drawn-scene-text">
      {words ? (
        renderChild(words)
      ) : (
        objects.length > 0 && (
          <dl className="drawn-scene-objects">
            {objects.map((object) => (
              <div key={object.id} className="drawn-scene-object">
                <dt>{object.label}</dt>
                <dd>{object.state}</dd>
              </div>
            ))}
          </dl>
        )
      )}
      {(scene.panels ?? []).map((panel) => {
        const held = childByKey(node, panel.key);
        if (!held) return null;
        return (
          <section key={panel.key} className="drawn-scene-panel-text" aria-label={anchorLabel(scene, panel.anchor)}>
            <h4 className="drawn-scene-anchor">{anchorLabel(scene, panel.anchor)}</h4>
            {renderChild(held)}
          </section>
        );
      })}
    </div>
  );
}
