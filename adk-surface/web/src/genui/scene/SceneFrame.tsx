import { Component, Suspense, lazy, useCallback, useEffect, useRef, useState } from "react";
import type { ErrorInfo, ReactNode, RefObject } from "react";
import type { DrawnNode, DrawnScene } from "../../generated/api";
import { PAUSE_MOTION, PLAY_MOTION, SCENE_SHOWN_AS_TEXT, SHOW_AS_TEXT, SHOW_IN_3D } from "../../generated/words";
import { visible } from "../../safe";
import type { Values } from "../bindings";
import { acquire, touch, webglAvailable } from "./contexts";
import { moves, picking } from "./plan";
import { anchorLabel, childByKey, labelledObjects, stateToken, stateWord, statesOf } from "./sceneText";

const SceneCanvas = lazy(() => import("./SceneCanvas"));

/** What a scene's frame needs of the drawing it is in. */
export interface SceneFrameProps {
  /** The scene's node: its children are its panels, then its text view. */
  node: DrawnNode;
  scene: DrawnScene;
  /** Draws a held node as the home draws it. */
  renderChild: (node: DrawnNode) => ReactNode;
  /** The surface and node, for what the page remembers between versions. */
  memoryKey?: string;
  version?: number;
  /** The drawing's values, and how a pick changes one: the home's own field state. */
  values?: Values;
  change?: (path: string, value: unknown) => void;
  /** The form can't change now: it is sent, or an earlier version. */
  disabled?: boolean;
  /** The choice field picking fills, as the drawing shows it. */
  field?: DrawnNode;
}

/** Whether the person asked for less motion. */
function useReducedMotion(): boolean {
  const query = typeof window !== "undefined" ? window.matchMedia?.("(prefers-reduced-motion: reduce)") : undefined;
  const [reduced, setReduced] = useState(() => query?.matches ?? false);
  useEffect(() => {
    if (!query) return;
    const changed = () => setReduced(query.matches);
    query.addEventListener?.("change", changed);
    return () => query.removeEventListener?.("change", changed);
  }, [query]);
  return reduced;
}

/** Whether `element` has come into view; the 3D view mounts only then. */
function useSeen<T extends Element>(): [RefObject<T>, boolean] {
  const ref = useRef<T>(null);
  const [seen, setSeen] = useState(() => typeof IntersectionObserver === "undefined");
  useEffect(() => {
    const element = ref.current;
    if (seen || !element || typeof IntersectionObserver === "undefined") return;
    const observer = new IntersectionObserver(([entry]) => {
      if (entry?.isIntersecting) {
        setSeen(true);
        observer.disconnect();
      }
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, [seen]);
  return [ref, seen];
}

/** Falls back to the scene's words if the 3D view throws. */
class Guard extends Component<{ onFail: (why: string) => void; children: ReactNode }, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidCatch(error: Error, _info: ErrorInfo) {
    this.props.onFail(error.message);
  }

  render() {
    return this.state.failed ? null : this.props.children;
  }
}

/**
 * A 3D scene an agent drew (spec 3.3 §6.1 `genui`, extension catalogs): the server's sentence
 * for it as a visible caption, the legend of its states (a word and a colour), what picking
 * fills, and the scene, drawn with WebGL once it is in view. Its words stand in where WebGL is
 * missing or fails, when too many scenes are live, and whenever the person asks for them.
 * Picking an object fills the drawing's choice field, which stays the way to choose by
 * keyboard and screen reader; nothing is sent.
 */
export function SceneFrame({ node, scene, renderChild, memoryKey, version = 1, values, change, disabled, field }: SceneFrameProps) {
  const [capable] = useState(webglAvailable);
  const [failed, setFailed] = useState<string>();
  const [asText, setAsText] = useState(false);
  const [asleep, setAsleep] = useState(false);
  const [paused, setPaused] = useState(false);
  const [said, setSaid] = useState("");
  const reduced = useReducedMotion();
  const [figure, seen] = useSeen<HTMLElement>();
  const states = statesOf(scene);
  const drawn = capable && !failed && !asText && !asleep;
  const live = useRef<{ id: number; release: () => void }>();

  useEffect(() => {
    if (!drawn || !seen) return;
    const place = acquire(() => setAsleep(true));
    live.current = place;
    return () => {
      place.release();
      live.current = undefined;
    };
  }, [drawn, seen]);

  const path = scene.select?.path;
  const multiple = field?.field?.input === "choices";
  const pickable = !!path && !!change && !disabled;
  const onPick = useCallback(
    (value: string, label: string) => {
      if (!path || !change) return;
      change(path, picking(value, values?.[path], multiple));
      setSaid(`Picked: ${label}`);
    },
    [path, change, values, multiple],
  );
  const fail = useCallback((why: string) => setFailed(why || "failed"), []);
  const slowed = useCallback(() => setPaused(true), []);

  const words = childByKey(node, scene.textView);
  const panels = (scene.panels ?? []).flatMap((panel) => {
    const held = childByKey(node, panel.key);
    return held ? [{ key: panel.key, anchor: panel.anchor, content: renderChild(held) }] : [];
  });
  const showsMotion = drawn && moves(scene) && !reduced;
  const stageHeight = Math.min(960, Math.max(200, scene.height ?? 380));

  return (
    <figure ref={figure} className="drawn-scene" data-scene={drawn ? "3d" : "text"}>
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
      {drawn ? (
        <>
          {seen ? (
            <Guard onFail={fail}>
              <Suspense fallback={<div className="drawn-scene-stage drawn-scene-loading" style={{ height: stageHeight }} />}>
                <SceneCanvas
                  scene={scene}
                  memoryKey={memoryKey ?? node.key}
                  version={version}
                  picked={path ? values?.[path] : undefined}
                  pickable={pickable}
                  onPick={onPick}
                  panels={panels}
                  paused={paused}
                  reducedMotion={reduced}
                  onFail={fail}
                  onSlow={slowed}
                  onTouch={() => live.current && touch(live.current.id)}
                />
              </Suspense>
            </Guard>
          ) : (
            <div className="drawn-scene-stage drawn-scene-loading" style={{ height: stageHeight }} />
          )}
          {/* What the scene shows, in words, for screen readers. */}
          {words && <div className="drawn-visually-hidden">{renderChild(words)}</div>}
        </>
      ) : (
        <>
          {(failed || !capable) && <p className="drawn-scene-note">{SCENE_SHOWN_AS_TEXT}</p>}
          <SceneText node={node} scene={scene} renderChild={renderChild} />
        </>
      )}
      <div className="drawn-scene-controls">
        {capable && !failed && (
          <button
            type="button"
            className="drawn-scene-toggle"
            onClick={() => {
              setAsleep(false);
              setAsText(!(asText || asleep));
            }}
          >
            {drawn ? SHOW_AS_TEXT : SHOW_IN_3D}
          </button>
        )}
        {showsMotion && (
          <button type="button" className="drawn-scene-toggle" aria-pressed={paused} onClick={() => setPaused(!paused)}>
            {paused ? PLAY_MOTION : PAUSE_MOTION}
          </button>
        )}
        <span className="drawn-visually-hidden" aria-live="polite">
          {visible(said)}
        </span>
      </div>
    </figure>
  );
}

/** A scene in words: its text view, or its labelled objects; then its panels. */
export function SceneText({ node, scene, renderChild }: Pick<SceneFrameProps, "node" | "scene" | "renderChild">) {
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
