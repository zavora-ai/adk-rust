import type { ReactNode } from "react";
import type { DrawnDepth, DrawnNode } from "../generated/api";
import { BACK, FRONT } from "../generated/words";
import { visible } from "../safe";

export interface DepthViewProps {
  node: DrawnNode;
  depth: DrawnDepth;
  /** Draws a held node as the home draws it. */
  renderChild: (node: DrawnNode) => ReactNode;
}

/**
 * Components presented in depth (spec 3.3 §6.1 `genui`, extension catalogs). This build lays
 * them out flat, in order: a flip card's front and back under those words, the others under
 * their titles.
 */
export function DepthView({ node, depth, renderChild }: DepthViewProps) {
  const children = node.children ?? [];
  const title = (index: number) =>
    depth.effect === "flip" ? (index === 0 ? FRONT : BACK) : depth.titles?.[index] ? visible(depth.titles[index]) : undefined;
  return (
    <div className="drawn-depth" data-effect={depth.effect} data-layout="flat">
      {children.map((child, index) => (
        <section key={child.key} className="drawn-depth-item">
          {title(index) && <h4 className="drawn-depth-title">{title(index)}</h4>}
          {renderChild(child)}
        </section>
      ))}
    </div>
  );
}
