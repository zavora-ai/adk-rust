// The annotated viewer (spec 3.7 §8.2, X12) over the boards' fixture content, for `npm run dev`
// at /ui/fixture.html?screen=viewer&viewer=<state> (and `screen=viewerPhone`) and the
// screenshots beside the signed-off boards. Each state is the engine's own projection of the
// answer records in `adk-surface/tests/fixtures/viewer/`, written by `viewer_tests`
// (`ADK_SURFACE_WRITE_VIEWER=1`); the pictures are the boards' drawings. Never part of the
// built page.
import type { AgentRow, LabelledExample, SavedExample, StatusLine, ViewerView } from "../generated/api";
import { ApiError } from "../client";
import type { PhoneScreen } from "../phone/route";
import type { ViewerClient } from "../viewer/Viewer";
import type { Correction } from "../viewer/Viewer";
import type { ViewerPlace } from "../viewer/route";
import { agents, status as homeStatus, FixtureClient } from "./fixture";
import { FixturePhoneClient } from "./phone";
import compareView from "./viewer/compare.view.json";
import documentView from "./viewer/document.view.json";
import emptyView from "./viewer/empty.view.json";
import photoView from "./viewer/photo.view.json";
import videoView from "./viewer/video.view.json";
import invoicePicture from "./viewer/invoice-inv-2291-p1.svg";
import framePicture from "./viewer/line-2-10-41-07.svg";
import frameOkPicture from "./viewer/line-2-11-02-44.svg";
import warehousePicture from "./viewer/warehouse-aisle-3.svg";

export type ViewerState = "document" | "photo" | "video" | "compare" | "empty" | "correct";

const VIEWS: Record<Exclude<ViewerState, "correct">, ViewerView> = {
  document: documentView as ViewerView,
  photo: photoView as ViewerView,
  video: videoView as ViewerView,
  compare: compareView as ViewerView,
  empty: emptyView as ViewerView,
};

const PICTURES: Record<string, string> = {
  "sha256:556b495de3f3b8c95e33b8ee5511552ada624e85f74b602af3ee2baea8a22654": invoicePicture,
  "sha256:594a57e5212677f5d8ed71e730129b7ca43fbf545c5189b27f4c769695c971ae": framePicture,
  "sha256:45e055ba4f630e4bfa506f38fb8506265e6774f6b7767bbfd482ec0272440903": frameOkPicture,
  "sha256:4484960c0d621670c96be084aa892ae1a277890c601b454f880377d8b3fd55ed": warehousePicture,
};

/** The picture an image route names, by the digest at its end. */
async function picture(href: string): Promise<Blob> {
  const digest = decodeURIComponent(href.split("/").pop() ?? "").split("?")[0];
  const url = PICTURES[digest];
  if (!url) throw new ApiError(404, "no such image", "surface.not_found");
  return (await fetch(url)).blob();
}

/** The state an address names: a comparison when it asks for one. */
function stateOf(place: ViewerPlace, first: ViewerState): Exclude<ViewerState, "correct"> {
  if (place.compare) return "compare";
  const shown = first === "correct" || first === "compare" ? "document" : first;
  const view = VIEWS[shown];
  return view.sessionId === place.session ? shown : "document";
}

function viewerClient(first: ViewerState): ViewerClient {
  return {
    viewer: async (place) => VIEWS[stateOf(place, first)],
    regions: async () => ({}),
    saveExample: async (_session: string, example: LabelledExample): Promise<SavedExample> => ({
      file: example.file,
      caseName: example.caseName,
      image: "images/556b495de3f3b8c95e33b8ee5511552ada624e85f74b602af3ee2baea8a22654.png",
      text: `Saved to ${example.file} as ${example.caseName}. The run's answer stays as it was.`,
    }),
  };
}

/** Where the state opens. */
export function viewerPlaceOf(state: ViewerState): ViewerPlace {
  const view = state === "correct" ? VIEWS.document : VIEWS[state];
  return {
    session: view.sessionId,
    image: view.image.digest,
    answer: state === "compare" ? view.compare?.a.answer.id : undefined,
    compare: state === "compare" ? view.compare?.b.answer.id : undefined,
  };
}

/** Correcting region 5 of the invoice, as `viewer-correct-web` draws it (board choice V13). */
export const CORRECTING: Correction = {
  index: 4,
  label: "handwriting",
  text: "PO-77815",
  box: [1252, 470, 1500, 536],
  was: [1250, 466, 1532, 540],
  wasText: "PO-7781S",
};

const DESK = { name: "desk", displayName: "Document desk", avatar: { monogram: "D", color: "#2563EB" } };
const INSPECTOR = { name: "inspector", displayName: "Inspector", avatar: { monogram: "I", color: "#7C3AED" } };

/** The web home with the viewer open on `state`. */
export class ViewerHomeFixture extends FixtureClient {
  constructor(private readonly state: ViewerState) {
    super();
  }

  override rows(): AgentRow[] {
    return [
      { agent: agents.chief, status: "working", statusText: "Working", sessionId: "s-chief" },
      { agent: DESK, status: "done", statusText: "Done", sessionId: "s-desk" },
      { agent: INSPECTOR, status: "working", statusText: "Watching line 2", sessionId: "s-line" },
      { agent: agents.research, status: "done", statusText: "Done", sessionId: "s-research" },
    ];
  }

  override async status(session?: string): Promise<StatusLine> {
    void session;
    return { ...homeStatus(false), decisions: 0 };
  }

  override async decisions() {
    return [];
  }

  async image(href: string): Promise<Blob> {
    return picture(href);
  }

  viewerClient(): ViewerClient {
    return viewerClient(this.state);
  }
}

/** The phone with the read-only viewer open on the line 2 still (board choice V15). */
export function viewerPhoneFixture(): { client: FixturePhoneClient; initial: PhoneScreen } {
  const client = new FixturePhoneClient(true);
  const views = viewerClient("photo");
  Object.assign(client, { image: picture, viewer: views.viewer });
  return { client, initial: { screen: "viewer", place: viewerPlaceOf("photo") } };
}
