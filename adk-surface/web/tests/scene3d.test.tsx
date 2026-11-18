// The 3D view of a scene and the depth effects (spec 3.3 §6.1 `genui`, extension catalogs):
// the pure plan (palette, camera, motion, picking), the frame around the lazily loaded canvas
// (words where WebGL is missing, the text toggle, pause, a pick that fills the drawing's field
// and says so, never sends), and depth effects whose hidden faces can't be reached.
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DrawnNode, DrawnScene, DrawnSurface } from "../src/generated/api";
import {
  NEXT,
  PAUSE_MOTION,
  PLAY_MOTION,
  PREVIOUS,
  SCENE_SHOWN_AS_TEXT,
  SHOW_AS_TEXT,
  SHOW_BACK,
  SHOW_FRONT,
  SHOW_IN_3D,
} from "../src/generated/words";
import { DepthView } from "../src/genui/DepthView";
import { DrawnView } from "../src/genui/DrawnView";
import { timesheetCard } from "../src/fixture/genui";
import { cameraFor, motionAt, objectColor, picking, rampColor, readPalette, seriesColor, tween } from "../src/genui/scene/plan";
import type { Pose } from "../src/genui/scene/plan";
import { MAX_LIVE, acquire } from "../src/genui/scene/contexts";

// The canvas stands in for WebGL, which jsdom lacks: it offers each pickable object as a button.
vi.mock("../src/genui/scene/SceneCanvas", () => ({
  default: (props: { scene: DrawnScene; pickable: boolean; onPick?: (value: string, label: string) => void; paused: boolean }) => (
    <div data-testid="canvas" data-paused={String(props.paused)}>
      {props.scene.objects
        .filter((object) => object.value)
        .map((object) => (
          <button
            key={object.id}
            type="button"
            disabled={!props.pickable}
            onClick={() => props.onPick?.(object.value!, object.label ?? "")}
          >
            pick {object.id}
          </button>
        ))}
    </div>
  ),
}));

const leaf = (key: string, text: string): DrawnNode => ({ key, component: "Text", text });

describe("the plan of a scene", () => {
  it("draws in the home's palette, states first, and falls back on its own colours", () => {
    const palette = readPalette(null);
    expect(objectColor({ color: "chart2", finish: "matte" }, null, palette)).toBe("#0d9488");
    expect(objectColor({ color: "chart2", finish: "matte" }, "critical", palette)).toBe("#b91c1c");
    expect(seriesColor(9, palette)).toBe(palette["--chart-2"]);
    expect(rampColor(0, palette)).toBe("#1e3a8a");
    expect(rampColor(1, palette)).toBe("#ca8a04");
  });

  it("frames the scene from its preset's side, or from where it is told", () => {
    const fitted = cameraFor({ camera: { preset: "top" } }, [0, 0, 0], 5);
    expect(fitted.position[1]).toBeGreaterThan(10);
    expect(Math.abs(fitted.position[0])).toBeLessThan(0.1);
    const told = cameraFor({ camera: { preset: "fit", at: [1, 2, 3], target: [0, 1, 0], fov: 30 } }, [9, 9, 9], 5);
    expect(told).toEqual({ position: [1, 2, 3], target: [0, 1, 0], fov: 30 });
  });

  it("moves an object by its motion and rests it once its repeats are done", () => {
    const rest: Pose = { position: [2, 0, 0], rotation: [0, 0, 0], scale: [1, 1, 1] };
    expect(motionAt({ target: "a", kind: "spin", axis: "y", seconds: 4 }, rest, 1)!.rotation[1]).toBeCloseTo(Math.PI / 2);
    expect(motionAt({ target: "a", kind: "bob", axis: "y", seconds: 4, amount: 0.5 }, rest, 1)!.position[1]).toBeCloseTo(0.5);
    const orbit = motionAt({ target: "a", kind: "orbit", axis: "y", seconds: 4 }, rest, 1)!;
    expect(Math.hypot(orbit.position[0], orbit.position[2])).toBeCloseTo(2);
    const path = motionAt({ target: "a", kind: "path", axis: "y", seconds: 2, path: [0, 0, 0, 10, 0, 0] }, rest, 1)!;
    expect(path.position[0]).toBeCloseTo(5);
    expect(motionAt({ target: "a", kind: "pulse", axis: "y", seconds: 1, repeat: 2 }, rest, 3)).toBeNull();
    expect(tween(0, 10, 1)).toBe(10);
  });

  it("picks one value for a choice and toggles values for choices", () => {
    expect(picking("dock-3", ["dock-1"], false)).toEqual(["dock-3"]);
    expect(picking("dock-3", ["dock-1"], true)).toEqual(["dock-1", "dock-3"]);
    expect(picking("dock-1", ["dock-1"], true)).toEqual([]);
  });

  it("keeps at most a handful of scenes drawing, sending the oldest to sleep", () => {
    const slept: number[] = [];
    const places = Array.from({ length: MAX_LIVE + 2 }, (_, index) => acquire(() => slept.push(index)));
    expect(slept).toEqual([0, 1]);
    for (const place of places) place.release();
  });
});

function yard(scene: Partial<DrawnScene> = {}): DrawnSurface {
  const drawn: DrawnScene = {
    kind: "space",
    objects: [
      {
        id: "dock-1",
        label: "Dock 1",
        shape: { type: "box", size: [1, 1, 1] },
        at: [0, 0, 0],
        look: { color: "chart1", finish: "matte" },
        value: "dock-1",
      },
      {
        id: "dock-3",
        label: "Dock 3",
        shape: { type: "box", size: [1, 1, 1] },
        at: [2, 0, 0],
        look: { color: "chart1", finish: "matte" },
        value: "dock-3",
      },
    ],
    camera: { preset: "fit" },
    lighting: "studio",
    select: { path: "/dock", note: "Picking one fills Dock. Nothing is sent." },
    summary: "3D scene of Dock 1 and Dock 3, 2 objects",
    motion: [{ target: "dock-3", kind: "pulse", axis: "y", seconds: 2 }],
    ...scene,
  };
  const root: DrawnNode = {
    key: "root",
    component: "Column",
    children: [
      {
        key: "map",
        component: "Space",
        extension: { catalog: "https://example.com/scenes.json", component: "Space", props: {}, children: [] },
        children: [{ key: "map#1", component: "Scene", scene: drawn, children: [] }],
      },
      {
        key: "dock",
        component: "ChoicePicker",
        text: "Dock",
        field: {
          path: "/dock",
          input: "choice",
          value: [],
          required: false,
          options: [
            { label: "Dock 1", value: "dock-1" },
            { label: "Dock 3", value: "dock-3" },
          ],
          checks: [],
        },
      } as unknown as DrawnNode,
    ],
  };
  return { ...timesheetCard(), title: undefined, surfaceId: "yard", root, actions: [] };
}

describe("a scene's frame", () => {
  const original = HTMLCanvasElement.prototype.getContext;
  afterEach(() => {
    HTMLCanvasElement.prototype.getContext = original;
  });

  it("shows the scene's words, and says so, where WebGL is missing", () => {
    HTMLCanvasElement.prototype.getContext = (() => null) as typeof original;
    const { container } = render(<DrawnView surface={yard()} />);
    expect(screen.getByText(SCENE_SHOWN_AS_TEXT)).toBeTruthy();
    expect(container.querySelector('[data-testid="canvas"]')).toBeNull();
    expect(screen.queryByText(SHOW_AS_TEXT)).toBeNull();
    expect(container.querySelector(".drawn-scene-objects")?.textContent).toContain("Dock 3");
  });

  describe("with WebGL", () => {
    beforeEach(() => {
      HTMLCanvasElement.prototype.getContext = (() => ({})) as unknown as typeof original;
    });

    it("draws in 3D once loaded; a pick fills the drawing's field, says so, and sends nothing", async () => {
      const onSend = vi.fn(async () => undefined);
      render(<DrawnView surface={yard()} onSend={onSend} />);
      const pick = await screen.findByText("pick dock-3");
      await act(async () => {
        fireEvent.click(pick);
      });
      const radio = screen.getByLabelText("Dock 3") as HTMLInputElement;
      expect(radio.checked).toBe(true);
      expect(screen.getByText("Picked: Dock 3")).toBeTruthy();
      expect(onSend).not.toHaveBeenCalled();
    });

    it("turns to its words and back, and pauses its motion", async () => {
      const { container } = render(<DrawnView surface={yard()} />);
      await screen.findByTestId("canvas");
      fireEvent.click(screen.getByText(PAUSE_MOTION));
      expect(screen.getByTestId("canvas").dataset.paused).toBe("true");
      expect(screen.getByText(PLAY_MOTION)).toBeTruthy();
      fireEvent.click(screen.getByText(SHOW_AS_TEXT));
      expect(container.querySelector('[data-testid="canvas"]')).toBeNull();
      expect(container.querySelector(".drawn-scene-objects")).toBeTruthy();
      fireEvent.click(screen.getByText(SHOW_IN_3D));
      expect(await screen.findByTestId("canvas")).toBeTruthy();
    });

    it("can't be picked from in an earlier version", async () => {
      render(<DrawnView surface={yard()} superseded />);
      const pick = (await screen.findByText("pick dock-1")) as HTMLButtonElement;
      expect(pick.disabled).toBe(true);
    });
  });
});

describe("depth effects", () => {
  const faces = [
    leaf("front", "p95 840 ms"),
    {
      key: "back",
      component: "TextField",
      text: "Reason",
      field: { path: "/reason", input: "shortText", value: "", required: false, options: [], checks: [] },
    } as unknown as DrawnNode,
  ];

  it("turn a flip card with the home's own button, its hidden face out of reach", () => {
    const { container } = render(
      <DepthView
        node={{ key: "card", component: "Depth", children: faces }}
        depth={{ effect: "flip" }}
        renderChild={(node) => <span>{node.text}</span>}
      />,
    );
    const back = container.querySelector('[data-index="1"]') as HTMLElement;
    expect(back.hasAttribute("inert")).toBe(true);
    fireEvent.click(screen.getByText(SHOW_BACK));
    expect(back.hasAttribute("inert")).toBe(false);
    expect((container.querySelector('[data-index="0"]') as HTMLElement).hasAttribute("inert")).toBe(true);
    expect(screen.getByText(SHOW_FRONT)).toBeTruthy();
  });

  it("bring a face holding a field with a problem to the front", () => {
    const { container } = render(
      <DepthView
        node={{ key: "card", component: "Depth", children: faces }}
        depth={{ effect: "flip" }}
        errors={{ "/reason": "Required" }}
        renderChild={(node) => <span>{node.text}</span>}
      />,
    );
    expect((container.querySelector('[data-index="1"]') as HTMLElement).hasAttribute("inert")).toBe(false);
  });

  it("turn a carousel with Previous and Next", () => {
    const items = [leaf("a", "One"), leaf("b", "Two"), leaf("c", "Three")];
    const { container } = render(
      <DepthView
        node={{ key: "ring", component: "Depth", children: items }}
        depth={{ effect: "carousel" }}
        renderChild={(node) => <span>{node.text}</span>}
      />,
    );
    const current = () =>
      within(container.querySelector(".drawn-depth-current") as HTMLElement).getByText(/One|Two|Three/).textContent;
    expect(current()).toBe("One");
    fireEvent.click(screen.getByText(NEXT));
    expect(current()).toBe("Two");
    fireEvent.click(screen.getByText(PREVIOUS));
    fireEvent.click(screen.getByText(PREVIOUS));
    expect(current()).toBe("Three");
  });
});
