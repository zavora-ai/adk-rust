// 3D scenes and depth effects agents draw (spec 3.3 §6.1 `genui`, extension catalogs): the
// server's sentence as a visible caption, states as a word and a colour, what picking fills,
// the scene's words (its text view, or its labelled objects) and its panels drawn by the home;
// children presented in depth, a flip card's faces under Front and Back.
import { render, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { DrawnNode, DrawnScene } from "../src/generated/api";
import { BACK, FRONT, STATE_CRITICAL, STATE_OK } from "../src/generated/words";
import { DrawnView } from "../src/genui/DrawnView";
import { timesheetCard } from "../src/fixture/genui";

const leaf = (key: string, text: string, variant?: string): DrawnNode => ({ key, component: "Text", text, variant });

/** A yard of docks: a floor, a fine dock and a critical one, a panel beside the critical one. */
export function yardScene(): DrawnScene {
  return {
    kind: "space",
    objects: [
      { id: "floor", shape: { type: "plane", width: 12, depth: 6 }, at: [0, 0, 0], look: { color: "muted", finish: "matte" } },
      {
        id: "dock-1",
        label: "Dock 1",
        shape: { type: "box", size: [2, 1, 2] },
        at: [-3, 0.5, 0],
        look: { color: "chart1", finish: "matte" },
        state: "ok",
        value: "dock-1",
      },
      {
        id: "dock-3",
        label: "Dock 3‮",
        shape: { type: "box", size: [2, 1, 2] },
        at: [3, 0.5, 0],
        look: { color: "chart1", finish: "matte" },
        state: "critical",
        value: "dock-3",
      },
    ],
    camera: { preset: "fit" },
    lighting: "studio",
    select: { path: "/dock", note: "Picking one fills Dock. Nothing is sent." },
    summary: "3D scene of Dock 1 and Dock 3, 3 objects",
    panels: [{ anchor: "dock-3", key: "note" }],
  };
}

function yard(scene: DrawnScene = yardScene(), children: DrawnNode[] = [leaf("note", "Closed since 09:00")]): DrawnNode {
  return {
    key: "map",
    component: "Space",
    extension: { catalog: "https://example.com/scenes.json", component: "Space", props: {}, children: ["note"] },
    children: [{ key: "map#1", component: "Scene", scene, children }],
  };
}

const surface = (root: DrawnNode) => ({ ...timesheetCard(), title: undefined, root });

describe("a 3D scene an extension component drew", () => {
  it("is captioned by the server's sentence, with its states as words and what picking fills", () => {
    const { container } = render(<DrawnView surface={surface(yard())} />);
    const figure = container.querySelector("figure.drawn-scene") as HTMLElement;
    expect(within(figure).getByText("3D scene of Dock 1 and Dock 3, 3 objects").tagName).toBe("FIGCAPTION");
    const legend = figure.querySelector(".drawn-scene-legend") as HTMLElement;
    expect(legend.textContent).toContain(STATE_OK);
    expect(legend.textContent).toContain(STATE_CRITICAL);
    expect(within(figure).getByText("Picking one fills Dock. Nothing is sent.")).toBeTruthy();
    expect(figure.querySelector("a, [href], img, iframe, foreignObject")).toBeNull();
    expect(container.querySelector('[class*="decision"], [class*="pill"]')).toBeNull();
  });

  it("lists its labelled objects in words, made safe to show, and its panels under their objects", () => {
    const { container } = render(<DrawnView surface={surface(yard())} />);
    const objects = container.querySelector(".drawn-scene-objects") as HTMLElement;
    expect(objects.textContent).toContain("Dock 1");
    expect(objects.textContent).not.toContain("‮");
    const panel = container.querySelector(".drawn-scene-panel-text") as HTMLElement;
    // A control character reads as its escape, never as the control.
    expect(panel.querySelector(".drawn-scene-anchor")?.textContent).toBe("Dock 3\\u{202e}");
    expect(panel.textContent).toContain("Closed since 09:00");
  });

  it("shows its text view in place of the objects when it has one", () => {
    const scene = { ...yardScene(), textView: "map#2" };
    const { container } = render(
      <DrawnView surface={surface(yard(scene, [leaf("note", "Closed since 09:00"), leaf("map#2", "Dock 1 is fine; dock 3 is closed.")]))} />,
    );
    expect(container.querySelector(".drawn-scene-objects")).toBeNull();
    expect(within(container).getByText("Dock 1 is fine; dock 3 is closed.")).toBeTruthy();
  });
});

describe("components presented in depth", () => {
  it("lays a flip card's faces out under Front and Back, drawn by the home", () => {
    const card: DrawnNode = {
      key: "card",
      component: "Card3D",
      extension: { catalog: "https://example.com/scenes.json", component: "Card3D", props: {}, children: ["front", "back"] },
      children: [{ key: "card#1", component: "Depth", depth: { effect: "flip" }, children: [leaf("front", "p95 840 ms"), leaf("back", "Up from 410 ms")] }],
    };
    const { container } = render(<DrawnView surface={surface(card)} />);
    const titles = [...container.querySelectorAll(".drawn-depth-title")].map((title) => title.textContent);
    expect(titles).toEqual([FRONT, BACK]);
    expect(container.textContent).toContain("Up from 410 ms");
  });
});
