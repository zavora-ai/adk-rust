// The annotated viewer (spec 3.7 §8.2, X12): the engine's projections of the boards' answers
// draw as the boards do — regions filterable by label, score and track; each region's backend,
// model, frame and transform chain; measurements; the run and call — and a person compares two
// backends, corrects a region and saves it as a labelled example, and exports the RegionSet.
// The phone reads the same view and saves nothing.
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ImageLoaderContext } from "../src/evidence/images";
import type { LabelledExample, SavedExample, ViewerView } from "../src/generated/api";
import { CORRECTING, viewerPlaceOf } from "../src/fixture/viewer";
import compareView from "../src/fixture/viewer/compare.view.json";
import documentView from "../src/fixture/viewer/document.view.json";
import emptyView from "../src/fixture/viewer/empty.view.json";
import photoView from "../src/fixture/viewer/photo.view.json";
import videoView from "../src/fixture/viewer/video.view.json";
import { phoneScreenOf } from "../src/phone/route";
import { PhoneViewer } from "../src/viewer/PhoneViewer";
import { maskPixels } from "../src/viewer/Stage";
import type { ViewerClient } from "../src/viewer/Viewer";
import { Viewer } from "../src/viewer/Viewer";
import { viewerOf, viewerPath } from "../src/viewer/route";
import type { ViewerPlace } from "../src/viewer/route";

const views: Record<string, ViewerView> = {
  document: documentView as ViewerView,
  compare: compareView as ViewerView,
  photo: photoView as ViewerView,
  empty: emptyView as ViewerView,
  video: videoView as ViewerView,
};

function loader(): Promise<Blob> {
  return Promise.resolve(new Blob([new Uint8Array([60, 115, 118, 103])], { type: "image/svg+xml" }));
}

function client(view: ViewerView, saved?: (example: LabelledExample) => void): ViewerClient & { regions: ReturnType<typeof vi.fn> } {
  return {
    viewer: vi.fn(async () => view),
    regions: vi.fn(async () => view.answer ?? {}),
    saveExample: vi.fn(async (_session: string, example: LabelledExample): Promise<SavedExample> => {
      saved?.(example);
      return { file: example.file, caseName: example.caseName, image: "images/x.png", text: `Saved to ${example.file} as ${example.caseName}. The run's answer stays as it was.` };
    }),
  };
}

function open(view: ViewerView, options: { correct?: boolean; viewerClient?: ViewerClient; onNavigate?: (place: ViewerPlace) => void } = {}) {
  const place = { session: view.sessionId, image: view.image.digest };
  const onOpenThread = vi.fn();
  render(
    <ImageLoaderContext.Provider value={loader}>
      <Viewer
        client={options.viewerClient ?? client(view)}
        place={place}
        onNavigate={options.onNavigate ?? vi.fn()}
        onOpenThread={onOpenThread}
        initialCorrection={options.correct ? CORRECTING : undefined}
      />
    </ImageLoaderContext.Provider>,
  );
  return { onOpenThread };
}

const panel = () => screen.getByRole("complementary", { name: "Answer" });

beforeEach(() => {
  globalThis.URL.createObjectURL = vi.fn(() => "blob:picture");
  globalThis.URL.revokeObjectURL = vi.fn();
});

describe("the annotated viewer", () => {
  it("draws a document page in reading order with the escalated value selected (V2, V4–V7)", async () => {
    open(views.document);
    await screen.findByRole("heading", { name: "INV-2291 · page 1" });
    expect(screen.getByText("sha256:556b49…a22654")).toBeTruthy();
    const side = panel();
    // The value replaced by an escalation is the region looked at first (board choice V6).
    const detail = within(side).getByRole("heading", { name: "5 · handwriting" }).closest("section")!;
    expect(within(detail as HTMLElement).getByText("PO-7781S")).toBeTruthy();
    expect(within(detail as HTMLElement).getByText("PO-77815")).toBeTruthy();
    expect(within(detail as HTMLElement).getByText("sha256:d4781e…272822")).toBeTruthy();
    expect(within(detail as HTMLElement).getByText("pp-ocrv6-small@3f2a9c1")).toBeTruthy();
    expect(within(detail as HTMLElement).getByText("Model-input pixels, 960 × 960, letterboxed")).toBeTruthy();
    expect(within(detail as HTMLElement).getByText("Padded 140 left, 141 right · 960 × 960")).toBeTruthy();
    // Numbered badges in reading order (V7) and the tag of the selected block only.
    const stage = screen.getByRole("img", { name: "INV-2291 · page 1" });
    expect(within(stage).getAllByText(/^\d+$/).map((badge) => badge.textContent)).toEqual(
      ["1", "2", "3", "4", "5", "6", "7", "8", "9", "11"],
    );
    expect(within(stage).getByText("5 · handwriting · 0.58")).toBeTruthy();
    expect(within(side).getByText("10 shown of 11 · 1 below 0.50")).toBeTruthy();
  });

  it("filters by label and score (V3)", async () => {
    open(views.document);
    await screen.findByRole("heading", { name: "INV-2291 · page 1" });
    const stage = screen.getByRole("img", { name: "INV-2291 · page 1" });
    const regions = () => within(stage).getAllByRole("button").length;
    expect(regions()).toBe(10);
    await userEvent.click(within(panel()).getByRole("button", { name: /^text\s*5$/ }));
    expect(regions()).toBe(5);
    expect(within(panel()).getByText("5 shown of 11 · 1 below 0.50")).toBeTruthy();
    const slider = within(panel()).getByRole("slider", { name: "Score at least" });
    await userEvent.click(within(panel()).getByRole("button", { name: /^text\s*5$/ }));
    // Lowering the score shows the 0.47 handwriting.
    fireEvent.change(slider, { target: { value: "0.4" } });
    expect(regions()).toBe(11);
    expect(within(panel()).getByText("11 shown of 11")).toBeTruthy();
  });

  it("shows a photo's masks, measurement and the run that measured it (V8, V9)", async () => {
    open(views.photo);
    await screen.findByRole("heading", { name: "Line 2 · part T43" });
    const side = panel();
    expect(within(side).getByText("14.3 mm")).toBeTruthy();
    expect(within(side).getByText("± 0.3 mm")).toBeTruthy();
    expect(
      within(side).getByText("In the belt's plane, from the fiducial: 40.0 mm is 245 px, so 6.12 px per mm. Valid only in that plane."),
    ).toBeTruthy();
    const detail = within(side).getByRole("heading", { name: "Seal tear" }).closest("section") as HTMLElement;
    expect(within(detail).getByText("sam2.1-tiny@8c1e2f0")).toBeTruthy();
    expect(within(detail).getByText("vision_measure")).toBeTruthy();
    expect(screen.getByText("Zoomed to 150% around the selected region", { selector: "figcaption" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Masks", pressed: true })).toBeTruthy();
  });

  it("decodes a mask in COCO's column-major order", () => {
    // A 4 × 3 raster with (1, 1) and (2, 1) set.
    const set = maskPixels({ width: 4, height: 3, counts: [4, 1, 2, 1, 4], area: 2 });
    expect([...set]).toEqual([0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0]);
  });

  it("filters a video frame by track, with the timeline and the anonymous-track note (V10)", async () => {
    open(views.video);
    await screen.findByRole("heading", { name: "Warehouse aisle 3 · 10:34.1" });
    const side = panel();
    expect(within(side).getByText("Tracks are anonymous and start again when the video is opened again. People are blurred in every kept frame.")).toBeTruthy();
    expect(screen.getByText("10:34.1 / 20:00")).toBeTruthy();
    const stage = screen.getByRole("img", { name: "Warehouse aisle 3 · 10:34.1" });
    expect(within(stage).getByText("person · blurred · T2 · 0.90")).toBeTruthy();
    const show = within(side).getByRole("heading", { name: "Show" }).closest("section") as HTMLElement;
    await userEvent.click(within(show).getByRole("button", { name: /^T2 person/ }));
    expect(within(stage).queryByText("person · blurred · T2 · 0.90")).toBeNull();
  });

  it("compares two backends and keeps one answer as a labelled example (V11)", async () => {
    const saved: LabelledExample[] = [];
    open(views.compare, { viewerClient: client(views.compare, (example) => saved.push(example)) });
    await screen.findByRole("heading", { name: "INV-2291 · page 1 · two answers" });
    const side = panel();
    expect(screen.getByText("14.8 s")).toBeTruthy();
    const counts = within(side).getByRole("heading", { name: "How they agree" }).closest("section") as HTMLElement;
    expect(within(counts).getByText("9")).toBeTruthy();
    expect(within(counts).getByText("2")).toBeTruthy();
    expect(within(counts).getByText("1")).toBeTruthy();
    expect(within(side).getByText("—")).toBeTruthy();
    await userEvent.click(within(side).getAllByRole("button", { name: "Keep PaddleOCR-VL's" })[0]);
    await userEvent.click(within(side).getByRole("button", { name: "Save example" }));
    await waitFor(() => expect(saved).toHaveLength(1));
    expect(saved[0]).toMatchObject({ answer: "inv-northwind-batch:c-34", scope: "region", region: { text: "PO-77815", label: "handwriting" } });
  });

  it("says an empty answer is an answer, apart from hidden and failed (V12)", async () => {
    const saved: LabelledExample[] = [];
    open(views.empty, { viewerClient: client(views.empty, (example) => saved.push(example)) });
    await screen.findByRole("heading", { name: "Nothing found" });
    const side = panel();
    expect(within(side).getByText("“torn seal”")).toBeTruthy();
    expect(within(side).getByText("$0.0025")).toBeTruthy();
    expect(within(side).getByText("The call failed.")).toBeTruthy();
    expect(within(side).getByText("Hidden by your filters.")).toBeTruthy();
    await userEvent.click(within(side).getByRole("button", { name: "Save as an example with nothing in it" }));
    await userEvent.click(within(side).getByRole("button", { name: "Save example" }));
    await waitFor(() => expect(saved).toHaveLength(1));
    expect(saved[0]).toMatchObject({ answer: "inv-line-2-c-2311:c-2311", scope: "nothing" });
    expect(saved[0].region).toBeUndefined();
  });

  it("corrects a region with the old box shown and saves it by digest (V13)", async () => {
    const saved: LabelledExample[] = [];
    open(views.document, { correct: true, viewerClient: client(views.document, (example) => saved.push(example)) });
    await screen.findByRole("heading", { name: "Correct region 5" });
    expect(screen.getByText("was")).toBeTruthy();
    expect(screen.getByText("your correction · handwriting")).toBeTruthy();
    expect(screen.getByText("1252, 470 → 1500, 536")).toBeTruthy();
    expect(screen.getByText("1250, 466 → 1532, 540")).toBeTruthy();
    expect(screen.getByText(/The case points at the image by its digest, sha256:556b49…a22654/)).toBeTruthy();
    expect(screen.getByText("150%")).toBeTruthy();
    await userEvent.click(screen.getByRole("radio", { name: "Every region on the page, with this correction" }));
    await userEvent.click(screen.getByRole("button", { name: "Save example" }));
    await waitFor(() => expect(saved).toHaveLength(1));
    expect(saved[0]).toEqual({
      answer: "inv-northwind-batch:c-31",
      file: "evals/vision/invoices.test.json",
      caseName: views.document.examples.caseName,
      scope: "page",
      region: { index: 4, label: "handwriting", text: "PO-77815", box: [1252, 470, 1500, 536] },
    });
    const said = `Saved to evals/vision/invoices.test.json as ${views.document.examples.caseName}. The run's answer stays as it was.`;
    expect(await screen.findByText(said)).toBeTruthy();
  });

  it("exports the RegionSet and opens the comparison by address (V11, V14)", async () => {
    const navigate = vi.fn();
    const viewerClient = client(views.document);
    open(views.document, { viewerClient, onNavigate: navigate });
    await screen.findByRole("heading", { name: "INV-2291 · page 1" });
    await userEvent.click(screen.getByRole("button", { name: "Export RegionSet" }));
    expect(viewerClient.regions).toHaveBeenCalledWith("s-desk", "inv-northwind-batch:c-31", undefined);
    await userEvent.click(screen.getByRole("button", { name: "Compare backends" }));
    expect(navigate).toHaveBeenCalledWith({
      session: "s-desk",
      image: views.document.image.digest,
      answer: "inv-northwind-batch:c-31",
      compare: "inv-northwind-batch:c-34",
    });
  });

  it("goes back to the agent's thread", async () => {
    const { onOpenThread } = open(views.document);
    await userEvent.click(await screen.findByRole("button", { name: "Document desk" }));
    expect(onOpenThread).toHaveBeenCalledWith("desk", "s-desk");
  });
});

describe("the viewer's address", () => {
  it("round-trips a session, a digest and the answers", () => {
    const place = viewerPlaceOf("compare");
    const path = viewerPath(place);
    expect(path.startsWith("/ui/vision/s-compare/sha256%3A")).toBe(true);
    const [pathname, search] = path.split("?");
    expect(viewerOf({ pathname, search: `?${search}` })).toEqual(place);
    expect(viewerOf({ pathname: "/ui/vision/", search: "" })).toBeUndefined();
    expect(phoneScreenOf({ pathname, hash: "", search: `?${search}` })).toEqual({ screen: "viewer", place });
  });
});

describe("the phone viewer", () => {
  it("is read-only and opens a tapped region's sheet (V15)", async () => {
    render(
      <ImageLoaderContext.Provider value={loader}>
        <PhoneViewer
          client={{ viewer: async () => views.photo }}
          place={viewerPlaceOf("photo")}
          onBack={vi.fn()}
          onOpenThread={vi.fn()}
        />
      </ImageLoaderContext.Provider>,
    );
    await screen.findByRole("heading", { name: "Line 2 · part T43" });
    expect(screen.getByText("Read-only on a phone. Correct, compare and export on a computer.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Correct this region|Compare|Export/ })).toBeNull();
    const rows = screen.getAllByRole("button", { name: /seal tear/ });
    await userEvent.click(rows[rows.length - 1]);
    const sheet = screen.getByRole("dialog", { name: "Seal tear" });
    expect(within(sheet).getByText("14.3 mm")).toBeTruthy();
    expect(within(sheet).getByText("Mask mapped back to source pixels")).toBeTruthy();
    expect(within(sheet).getByText("Model-input pixels, 1024 × 1024, letterboxed")).toBeTruthy();
  });
});
