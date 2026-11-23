// The live camera surface (spec 3.7 §8.2): the engine's projections of the boards' states draw
// as the boards do — live frames with overlays, tracks, watch conditions and zones, the frame
// governor's state and the source switch — and a person switches the source off, edits a zone,
// stops the stream and raises a card from a frozen frame. The phone reads the same view.
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { Camera } from "../src/camera/Camera";
import type { CameraCapture } from "../src/camera/capture";
import type { CameraClient } from "../src/camera/client";
import { PhoneCamera } from "../src/camera/PhoneCamera";
import { cameraOf, cameraPath } from "../src/camera/route";
import { ImageLoaderContext } from "../src/evidence/images";
import type { CameraView, CamerasView, StageView } from "../src/generated/api";
import later from "../src/fixture/camera/later-stage.json";
import live from "../src/fixture/camera/live.json";
import quotaExhausted from "../src/fixture/camera/quota-exhausted.json";
import several from "../src/fixture/camera/several-sources.json";
import sourceLost from "../src/fixture/camera/source-lost.json";
import steppedDown from "../src/fixture/camera/stepped-down.json";
import stopAll from "../src/fixture/camera/stop-all.json";
import switchedOff from "../src/fixture/camera/switched-off.json";
import { phoneScreenOf, phonePathOf } from "../src/phone/route";

const views = {
  live: live as CameraView,
  steppedDown: steppedDown as CameraView,
  quotaExhausted: quotaExhausted as CameraView,
  sourceLost: sourceLost as CameraView,
  switchedOff: switchedOff as CameraView,
};

function loader(): Promise<Blob> {
  return Promise.resolve(new Blob([new Uint8Array([60, 115, 118, 103])], { type: "image/svg+xml" }));
}

type Mocked = CameraClient & Record<string, ReturnType<typeof vi.fn>> & {
  saveZone: ReturnType<typeof vi.fn>;
  decide: ReturnType<typeof vi.fn>;
};

function client(view: CameraView): Mocked {
  return {
    cameras: vi.fn(async () => several as CamerasView),
    camera: vi.fn(async () => view),
    switchTo: vi.fn(async (_id: string, on: boolean) => (on ? views.live : views.switchedOff)),
    stop: vi.fn(async () => view),
    retry: vi.fn(async () => view),
    saveZone: vi.fn(async () => ({ zone: view.zones[0], text: "Saved. The seal tear watch uses it from the next frame." })),
    freeze: vi.fn(async () => later as StageView),
    raiseCard: vi.fn(async () => ({
      digest: "sha256:ab",
      shortDigest: "sha256:ab",
      regions: 5,
      keptUntil: "Kept as evidence until 18 Dec",
      href: "/ui/vision/s-line/vision-ab.png",
      outcome: { type: "sent", sessionId: "s-line" },
      text: "Raised to the Inspector. This one frame is kept as evidence until 18 Dec.",
    })),
    decide: vi.fn(async () => ({ type: "answered" })),
  } as unknown as Mocked;
}

function open(view: CameraView, camera = client(view), place = { camera: "line-2" }) {
  const handlers = { onNavigate: vi.fn(), onOpenThread: vi.fn(), onOpenDecision: vi.fn(), onMemory: vi.fn() };
  render(
    <ImageLoaderContext.Provider value={loader}>
      <Camera client={camera} place={place} {...handlers} />
    </ImageLoaderContext.Provider>,
  );
  return { camera, ...handlers };
}

const aside = () => screen.getByRole("complementary", { name: "Inspector" });

beforeEach(() => {
  globalThis.URL.createObjectURL = vi.fn(() => "blob:picture");
  globalThis.URL.revokeObjectURL = vi.fn();
});

describe("the live camera surface", () => {
  it("draws live frames with overlays, the governor, watches, tracks and sources (C1, C3, C10, C15)", async () => {
    open(views.live);
    await screen.findByRole("heading", { name: "Line 2 camera" });
    expect(screen.getByText("Live · 10:41:09")).toBeTruthy();
    expect(screen.getByText("Looking 5 a second · overlays 0.1 s behind")).toBeTruthy();
    for (const tag of ["part · T45", "seal tear · 0.91 · 5 frames · 14.3 mm", "fiducial", "Inspection window"]) {
      expect(screen.getAllByText(tag).length).toBeGreaterThan(0);
    }
    const side = aside();
    expect(within(side).getByText("Asleep until a defect")).toBeTruthy();
    expect(within(side).getByText("Stop line 2")).toBeTruthy();
    expect(within(side).getByRole("button", { name: "Confirm it's you and approve" })).toBeTruthy();
    expect(within(side).getByText("Woke the Inspector 3 times today")).toBeTruthy();
    expect(within(side).getByText("61,240 of 300,000")).toBeTruthy();
    expect(within(side).getByText("At most 6 a minute · 14 today · $0.14")).toBeTruthy();
    expect(within(side).getByText(/Steps down in this order/)).toBeTruthy();
    expect(within(side).getByText("seal tear 0.91 · in the zone · 5 frames")).toBeTruthy();
    expect(screen.getByText("This computer's camera")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Camera on" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("switches the source off and says nothing is captured or sent (C2, C7)", async () => {
    const { camera } = open(views.live);
    await screen.findByText("Live · 10:41:09");
    await userEvent.click(screen.getByRole("button", { name: "Off" }));
    expect(camera.switchTo).toHaveBeenCalledWith("line-2", false);
    expect(await screen.findByText("Camera off", { selector: ".cam-cover-title" })).toBeTruthy();
    expect(screen.getByText("Nothing is captured or sent. You switched line 2 off at 11:05.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Raise a card from this frame" })).toHaveProperty("disabled", true);
  });

  it("shows the governor stepped down with each step and the next (C4)", async () => {
    open(views.steppedDown);
    expect(await screen.findByText("Saving frames.")).toBeTruthy();
    const side = aside();
    expect(within(side).getByText("Lowered from 5 at 14:02")).toBeTruthy();
    expect(within(side).getByText("Looked less often: 5 → 2 a second (90% used)")).toBeTruthy();
    expect(within(side).getByText("At 100%: pause the camera until midnight")).toBeTruthy();
    expect(within(side).getByText("Nothing is waiting for you.")).toBeTruthy();
  });

  it("pauses at the quota with the limit a click away (C5)", async () => {
    const { onMemory } = open(views.quotaExhausted);
    expect(await screen.findByText("Parts on line 2 are passing without inspection.")).toBeTruthy();
    expect(screen.getByText("Paused until midnight", { selector: ".cam-cover-title" })).toBeTruthy();
    expect(screen.getByText("Last frame · 15:12:03")).toBeTruthy();
    await userEvent.click(within(aside()).getByRole("button", { name: "Change the daily frame limit" }));
    expect(onMemory).toHaveBeenCalled();
    expect(screen.getByText("No tracks while paused.")).toBeTruthy();
  });

  it("says Stop all paused the camera and offers nothing to act on (spec 3.7 V-I17)", async () => {
    open(stopAll as CameraView);
    expect(await screen.findByText("Paused by Stop all", { selector: ".cam-cover-title" })).toBeTruthy();
    expect(screen.getByText("Nothing is looked at or sent until someone chooses Resume all.")).toBeTruthy();
    expect(within(aside()).getByText("Not watching: Stop all")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Raise a card from this frame" })).toHaveProperty("disabled", true);
  });

  it("shows a lost source with its credentials hidden and tries again (C6)", async () => {
    const { camera } = open(views.sourceLost);
    expect(await screen.findByText("Can't reach the line 2 camera")).toBeTruthy();
    expect(screen.getByText(/rtsp:\/\/\[credentials hidden\]@10\.0\.4\.21/)).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Try again now" }));
    expect(camera.retry).toHaveBeenCalledWith("line-2");
  });

  it("edits a zone with the old outline shown and saves its corners (C12)", async () => {
    const { camera } = open(views.live);
    await screen.findByText("Live · 10:41:09");
    await userEvent.click(screen.getByRole("button", { name: "Draw a zone" }));
    const dialog = await screen.findByRole("dialog", { name: "Draw or edit a zone" });
    expect(within(dialog).getByText("Seal tear watch")).toBeTruthy();
    // The corners now and the corners it was, both in the camera's pixels.
    expect(within(dialog).getAllByText("760, 170 → 1560, 910")).toHaveLength(2);
    const stage = dialog.querySelector(".cam-editor-layer") as HTMLElement;
    stage.getBoundingClientRect = () => ({ left: 0, top: 0, width: 760, height: 427.5, right: 760, bottom: 427.5, x: 0, y: 0, toJSON: () => ({}) });
    const corner = dialog.querySelector('[data-corner="0,0"]') as HTMLElement;
    fireEvent.pointerDown(corner, { clientX: 300.8, clientY: 67.3, pointerId: 1 });
    fireEvent.pointerMove(stage, { clientX: 277.1, clientY: 79.2, pointerId: 1 });
    fireEvent.pointerUp(stage, { pointerId: 1 });
    expect(within(dialog).getByText("700, 200 → 1560, 910")).toBeTruthy();
    await userEvent.click(within(dialog).getByRole("button", { name: "Save zone" }));
    await waitFor(() => expect(camera.saveZone).toHaveBeenCalled());
    const [, zone, change] = camera.saveZone.mock.calls[0];
    expect(zone).toBe("inspection-window");
    expect(change.was).toEqual([760, 170, 1560, 910]);
    expect(change.corners.map(Math.round)).toEqual([700, 200, 1560, 910]);
    expect(await screen.findByText("Saved. The seal tear watch uses it from the next frame.")).toBeTruthy();
  });

  it("freezes the frame and raises a card with only that frame kept (C11)", async () => {
    const { camera } = open(views.live);
    await screen.findByText("Live · 10:41:09");
    await userEvent.click(screen.getByRole("button", { name: "Raise a card from this frame" }));
    expect(camera.freeze).toHaveBeenCalledWith("line-2", views.live.stage.frame!.digest);
    const dialog = await screen.findByRole("dialog", { name: "Raise a card from this frame" });
    expect(within(dialog).getByText("Frozen · 10:43:18")).toBeTruthy();
    expect(within(dialog).getByText("This one frame, as evidence for 30 days. Nothing else is stored.")).toBeTruthy();
    expect(within(dialog).getByText("The 5 on screen now go with it")).toBeTruthy();
    await userEvent.type(within(dialog).getByRole("textbox"), "The seal on T47 looks lifted at the edge.");
    await userEvent.click(within(dialog).getByRole("button", { name: "Raise the card" }));
    await waitFor(() =>
      expect(camera.raiseCard).toHaveBeenCalledWith("line-2", {
        frame: (later as StageView).frame!.digest,
        note: "The seal on T47 looks lifted at the edge.",
      }),
    );
    expect(await screen.findByText("Raised to the Inspector. This one frame is kept as evidence until 18 Dec.")).toBeTruthy();
  });

  it("stops the stream and approves the held line stop with its frame named (C2, C13)", async () => {
    const { camera } = open(views.live);
    await screen.findByText("Live · 10:41:09");
    await userEvent.click(screen.getByRole("button", { name: "Stop the stream" }));
    expect(camera.stop).toHaveBeenCalledWith("line-2");
    await userEvent.click(within(aside()).getByRole("button", { name: "Confirm it's you and approve" }));
    const [command] = camera.decide.mock.calls[0];
    expect(command).toMatchObject({ type: "decide", decisionId: "dec-line-2", answer: "allowOnce" });
    expect(command.evidence).toEqual([views.live.decisions[0].media!.images[0].digest]);
  });

  it("shows every source with its switch and a budget per source (C8)", async () => {
    const { camera, onNavigate } = open(views.live, client(views.live), {} as { camera: string });
    expect(await screen.findByRole("heading", { name: "Cameras" })).toBeTruthy();
    expect(screen.getByText("4 sources · 3 on")).toBeTruthy();
    expect(screen.getByText("Select a camera to watch it large. People are blurred before any frame is shown or kept.")).toBeTruthy();
    expect(screen.getAllByText("person · blurred").length).toBeGreaterThan(0);
    expect(screen.getByText("Line 3 · frames today")).toBeTruthy();
    await userEvent.click(screen.getByRole("group", { name: "This computer's camera: on or off" }).querySelector("button")!);
    expect(camera.switchTo).toHaveBeenCalledWith("this-computer", true);
    await userEvent.click(screen.getAllByText("Packing bench")[0].closest("button")!);
    expect(onNavigate).toHaveBeenCalledWith({ camera: "packing-bench" });
  });
});

describe("this browser's camera", () => {
  /** A camera that takes this browser's frames, its switch as the server keeps it. */
  function browserCamera(dropped?: "off") {
    let current: CameraView = { ...views.switchedOff, actions: { ...views.switchedOff.actions, sendFrames: true } };
    const camera = client(current);
    camera.camera = vi.fn(async () => current);
    camera.switchTo = vi.fn(async (_id: string, on: boolean) => {
      current = on
        ? { ...views.live, actions: { ...views.live.actions, sendFrames: true } }
        : { ...views.switchedOff, actions: { ...views.switchedOff.actions, sendFrames: true } };
      return current;
    });
    camera.sendFrame = vi.fn(async () => (dropped ? { taken: false, dropped } : { taken: true }));
    const track = { stop: vi.fn() };
    const stream = { getTracks: () => [track] } as unknown as MediaStream;
    const grabbed: { jpeg?: (frame: Blob) => void; stop: ReturnType<typeof vi.fn<() => void>> } = { stop: vi.fn<() => void>() };
    const capture = {
      open: vi.fn(async () => stream),
      frames: vi.fn((_stream: MediaStream, _every: number, jpeg: (frame: Blob) => void) => {
        grabbed.jpeg = jpeg;
        return grabbed.stop;
      }),
    };
    return { camera, capture, track, grabbed };
  }

  function openWith(camera: CameraClient, capture: CameraCapture) {
    render(
      <ImageLoaderContext.Provider value={loader}>
        <Camera
          client={camera}
          place={{ camera: "line-2" }}
          onNavigate={vi.fn()}
          onOpenThread={vi.fn()}
          onOpenDecision={vi.fn()}
          onMemory={vi.fn()}
          capture={capture}
        />
      </ImageLoaderContext.Provider>,
    );
  }

  it("switches the camera on, sends its frames, and off stops the browser's camera first", async () => {
    const { camera, capture, track, grabbed } = browserCamera();
    openWith(camera, capture);
    await userEvent.click(await screen.findByRole("button", { name: "Use this browser's camera" }));
    expect(camera.switchTo).toHaveBeenCalledWith("line-2", true);
    expect(capture.open).toHaveBeenCalled();
    expect(capture.frames.mock.calls[0][1]).toBe(200);
    expect(await screen.findByText("This browser's camera is sending frames. Off, or leaving this page, stops it.")).toBeTruthy();
    const frame = new Blob([new Uint8Array([0xff, 0xd8, 0xff])], { type: "image/jpeg" });
    grabbed.jpeg!(frame);
    await waitFor(() => expect(camera.sendFrame).toHaveBeenCalledWith("line-2", frame));

    await userEvent.click(screen.getByRole("button", { name: "Off" }));
    expect(track.stop).toHaveBeenCalled();
    expect(grabbed.stop).toHaveBeenCalled();
    expect(camera.switchTo).toHaveBeenLastCalledWith("line-2", false);
    expect(await screen.findByRole("button", { name: "Use this browser's camera" })).toBeTruthy();
    // A frame grabbed after the stop is never sent.
    grabbed.jpeg!(frame);
    expect(camera.sendFrame).toHaveBeenCalledTimes(1);
  });

  it("stops capturing when the server says the camera is off", async () => {
    const { camera, capture, track, grabbed } = browserCamera("off");
    openWith(camera, capture);
    await userEvent.click(await screen.findByRole("button", { name: "Use this browser's camera" }));
    grabbed.jpeg!(new Blob([new Uint8Array([0xff, 0xd8, 0xff])], { type: "image/jpeg" }));
    await waitFor(() => expect(track.stop).toHaveBeenCalled());
    expect(grabbed.stop).toHaveBeenCalled();
  });

  it("is offered only for a camera that takes this browser's frames", async () => {
    open(views.live);
    await screen.findByText("Live · 10:41:09");
    expect(screen.queryByRole("button", { name: "Use this browser's camera" })).toBeNull();
  });
});

describe("on a phone", () => {
  it("shows the stream, the switch, the decision, the budget rows and three tracks (C14)", async () => {
    const camera = client(views.live);
    render(
      <ImageLoaderContext.Provider value={loader}>
        <PhoneCamera client={camera} place={{ camera: "line-2" }} onBack={vi.fn()} onNavigate={vi.fn()} onOpenDecision={vi.fn()} />
      </ImageLoaderContext.Provider>,
    );
    expect(await screen.findByRole("heading", { name: "Line 2 camera" })).toBeTruthy();
    expect(screen.getByText("2 a minute · at most 6")).toBeTruthy();
    expect(screen.getByText("seal tear 0.91 · in the zone")).toBeTruthy();
    expect(document.querySelectorAll(".cam-track")).toHaveLength(3);
    expect(screen.getByRole("link", { name: "Edit the zone" }).getAttribute("href")).toBe("/ui/cameras/line-2?zone=edit");
    await userEvent.click(screen.getByRole("button", { name: "Off" }));
    expect(camera.switchTo).toHaveBeenCalledWith("line-2", false);
  });

  it("says a lost camera is checked by nothing and tries again", async () => {
    const camera = client(views.sourceLost);
    render(
      <ImageLoaderContext.Provider value={loader}>
        <PhoneCamera client={camera} place={{ camera: "line-2" }} onBack={vi.fn()} onNavigate={vi.fn()} onOpenDecision={vi.fn()} />
      </ImageLoaderContext.Provider>,
    );
    expect(await screen.findByText("Nothing is being checked.")).toBeTruthy();
    expect(screen.getByText("No frames since 10:42")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Try again now" }));
    expect(camera.retry).toHaveBeenCalledWith("line-2");
  });
});

describe("addresses", () => {
  it("opens a camera, the grid and the zone editor by address", () => {
    expect(cameraOf({ pathname: "/ui/cameras/line-2", search: "" })).toEqual({ camera: "line-2" });
    expect(cameraOf({ pathname: "/ui/cameras", search: "" })).toEqual({});
    expect(cameraOf({ pathname: "/ui/cameras/line-2", search: "?zone=edit" })).toEqual({ camera: "line-2", zone: true });
    expect(cameraOf({ pathname: "/ui/vision/s/x", search: "" })).toBeUndefined();
    expect(cameraPath({ camera: "line 2" })).toBe("/ui/cameras/line%202");
    expect(phoneScreenOf({ pathname: "/ui/cameras/line-2", hash: "" })).toEqual({ screen: "camera", place: { camera: "line-2" } });
    expect(phonePathOf({ screen: "camera", place: {} })).toBe("/ui/cameras");
  });
});
