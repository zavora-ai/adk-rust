import { useEffect, useRef, useState } from "react";
import type { CSSProperties, PointerEvent, ReactNode } from "react";
import type { DrawnDepth, DrawnNode } from "../generated/api";
import { BACK, FRONT, NEXT, PREVIOUS, SHOW_BACK, SHOW_FRONT } from "../generated/words";
import { visible } from "../safe";

export interface DepthViewProps {
  node: DrawnNode;
  depth: DrawnDepth;
  /** Fields that failed their checks, by data path: a face holding one turns to the front. */
  errors?: Record<string, string>;
  /** Draws a held node as the home draws it. */
  renderChild: (node: DrawnNode) => ReactNode;
}

/** Whether the person asked for less motion. */
function reducedMotion(): boolean {
  return typeof window !== "undefined" && !!window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
}

/** The data paths of the fields under `node`. */
function fieldPaths(node: DrawnNode): string[] {
  const out: string[] = [];
  const walk = (current: DrawnNode) => {
    if (current.field) out.push(current.field.path);
    for (const child of current.children ?? []) walk(child);
  };
  walk(node);
  return out;
}

/**
 * Components presented in depth (spec 3.3 §6.1 `genui`, extension catalogs), with CSS 3D and
 * no WebGL. A card tilts towards the pointer; a flip card turns over with the home's own button
 * and the hidden face is inert; layers stack; a carousel turns with Previous and Next, and
 * focusing an item brings it to the front; a wall curves; a stage turns as the person drags it
 * or presses the arrow keys. Under reduced motion, and on narrow screens, they lay out flat.
 */
export function DepthView({ node, depth, errors, renderChild }: DepthViewProps) {
  const children = node.children ?? [];
  const [front, setFront] = useState(0);
  const [turn, setTurn] = useState(0);
  const card = useRef<HTMLDivElement>(null);
  const drag = useRef<{ x: number; turn: number }>();
  const reduced = reducedMotion();
  const motion = reduced ? "reduced" : "full";
  const title = (index: number) =>
    depth.effect === "flip" ? (index === 0 ? FRONT : BACK) : depth.titles?.[index] ? visible(depth.titles[index]) : undefined;

  // A field with a problem is never left on a hidden face.
  useEffect(() => {
    if (!errors || (depth.effect !== "flip" && depth.effect !== "carousel")) return;
    const failing = children.findIndex((child) => fieldPaths(child).some((path) => errors[path]));
    if (failing >= 0 && failing !== front) setFront(failing);
  }, [errors]); // eslint-disable-line react-hooks/exhaustive-deps

  const tilt = (event: PointerEvent<HTMLDivElement>) => {
    const element = card.current;
    if (!element || reduced) return;
    const box = element.getBoundingClientRect();
    const x = (event.clientX - box.left) / box.width - 0.5;
    const y = (event.clientY - box.top) / box.height - 0.5;
    element.style.setProperty("--tilt-x", `${(-y * 8).toFixed(2)}deg`);
    element.style.setProperty("--tilt-y", `${(x * 10).toFixed(2)}deg`);
  };
  const untilt = () => {
    card.current?.style.setProperty("--tilt-x", "0deg");
    card.current?.style.setProperty("--tilt-y", "0deg");
  };
  const turnStage = (next: number) => {
    const clamped = Math.max(-35, Math.min(35, next));
    setTurn(clamped);
    card.current?.style.setProperty("--stage-turn", `${clamped.toFixed(1)}deg`);
  };

  // How far an item stands from the middle (a wall) or from the front (a carousel, the
  // shorter way round), and how far either way, for the styles.
  const offset = (index: number) => {
    if (depth.effect === "carousel") {
      const count = Math.max(1, children.length);
      const half = Math.floor(count / 2);
      return ((index - front + count + half) % count) - half;
    }
    return index - (children.length - 1) / 2;
  };
  const item = (child: DrawnNode, index: number, extra?: { hidden?: boolean; className?: string; onFocus?: () => void }) => (
    <section
      key={child.key}
      className={`drawn-depth-item ${extra?.className ?? ""}`}
      data-index={index}
      style={{ "--i": index, "--n": children.length, "--d": offset(index), "--ad": Math.abs(offset(index)) } as CSSProperties}
      // A face turned away can't be reached by keyboard or pointer.
      {...(extra?.hidden ? { inert: "", "aria-hidden": true } : {})}
      onFocus={extra?.onFocus}
    >
      {title(index) && <h4 className="drawn-depth-title">{title(index)}</h4>}
      {renderChild(child)}
    </section>
  );

  switch (depth.effect) {
    case "tilt":
      return (
        <div className="drawn-depth" data-effect="tilt" data-motion={motion}>
          <div ref={card} className="drawn-depth-card" onPointerMove={tilt} onPointerLeave={untilt}>
            {children.map((child, index) => item(child, index))}
          </div>
        </div>
      );
    case "flip":
      return (
        <div className="drawn-depth" data-effect="flip" data-face={front === 0 ? "front" : "back"} data-motion={motion}>
          <div className="drawn-depth-card">
            {children.map((child, index) =>
              item(child, index, { hidden: index !== front, className: index === 0 ? "drawn-depth-front" : "drawn-depth-back" }),
            )}
          </div>
          <button type="button" className="drawn-depth-turn" onClick={() => setFront(front === 0 ? 1 : 0)}>
            {front === 0 ? SHOW_BACK : SHOW_FRONT}
          </button>
        </div>
      );
    case "layers":
      return (
        <div className="drawn-depth" data-effect="layers" data-motion={motion}>
          <div className="drawn-depth-stack">
            {children.map((child, index) => item(child, index, { className: "drawn-depth-layer" }))}
          </div>
        </div>
      );
    case "carousel": {
      const count = Math.max(1, children.length);
      return (
        <div className="drawn-depth" data-effect="carousel" data-motion={motion}>
          <div className="drawn-depth-ring">
            {children.map((child, index) =>
              item(child, index, {
                className:
                  index === front
                    ? "drawn-depth-current"
                    : Math.abs(offset(index)) === 1
                      ? "drawn-depth-aside"
                      : "drawn-depth-away",
                onFocus: () => setFront(index),
              }),
            )}
          </div>
          {count > 1 && (
            <div className="drawn-depth-nav">
              <button type="button" className="drawn-depth-turn" onClick={() => setFront((front - 1 + count) % count)}>
                {PREVIOUS}
              </button>
              <span className="drawn-depth-count">
                {front + 1} / {count}
              </span>
              <button type="button" className="drawn-depth-turn" onClick={() => setFront((front + 1) % count)}>
                {NEXT}
              </button>
            </div>
          )}
        </div>
      );
    }
    case "wall":
      return (
        <div className="drawn-depth" data-effect="wall" data-count={children.length} data-motion={motion}>
          <div className="drawn-depth-wall">{children.map((child, index) => item(child, index))}</div>
        </div>
      );
    case "stage":
    default:
      return (
        <div className="drawn-depth" data-effect="stage" data-motion={motion}>
          <div
            className="drawn-depth-stage"
            tabIndex={0}
            ref={card}
            onPointerDown={(event) => {
              drag.current = { x: event.clientX, turn };
            }}
            onPointerMove={(event) => {
              if (!drag.current || reduced) return;
              turnStage(drag.current.turn + (event.clientX - drag.current.x) / 6);
            }}
            onPointerUp={() => {
              drag.current = undefined;
            }}
            onPointerLeave={() => {
              drag.current = undefined;
            }}
            onKeyDown={(event) => {
              if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
              event.preventDefault();
              turnStage(turn + (event.key === "ArrowLeft" ? -7 : 7));
            }}
          >
            {children.map((child, index) => item(child, index))}
          </div>
        </div>
      );
  }
}
