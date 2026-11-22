// The live camera surface (spec 3.7 §8.2) over the boards' fixture content, for `npm run dev`
// at /ui/fixture.html?screen=camera&camera=<state> (and `screen=cameraPhone`) and the
// screenshots beside the signed-off boards. Each state is the engine's own projection of the
// scripted cameras (`adk_surface::camera::ScriptedCameras`), written by `camera_tests`
// (`ADK_SURFACE_WRITE_CAMERA=1`); the pictures are the boards' drawings. Never part of the
// built page.
import type {
  AgentRow,
  CameraView,
  CamerasView,
  CardRaised,
  Command,
  CommandOutcome,
  Origin,
  RaiseCard as RaiseCardBody,
  StageView,
  StatusLine,
  ZoneChange,
  ZoneSaved,
} from "../generated/api";
import { ApiError } from "../client";
import type { CameraClient } from "../camera/client";
import type { PhoneScreen } from "../phone/route";
import type { Opened } from "../camera/Camera";
import type { CameraPlace } from "../camera/route";
import { ZoneEditor } from "../camera/ZoneEditor";
import { RaiseCard } from "../camera/RaiseCard";
import { ImageLoaderContext } from "../evidence/images";
import { agents, status as homeStatus, FixtureClient } from "./fixture";
import { FixturePhoneClient } from "./phone";
import live from "./camera/live.json";
import steppedDown from "./camera/stepped-down.json";
import quotaExhausted from "./camera/quota-exhausted.json";
import sourceLost from "./camera/source-lost.json";
import switchedOff from "./camera/switched-off.json";
import stopAll from "./camera/stop-all.json";
import several from "./camera/several-sources.json";
import later from "./camera/later-stage.json";
import pictures from "./camera/pictures.json";
import line2Live from "./camera/line-2-live.svg";
import line2Down from "./camera/line-2-down.svg";
import line2Paused from "./camera/line-2-paused.svg";
import line2Later from "./camera/line-2-later.svg";
import line3 from "./camera/line-3.svg";
import packingBench from "./camera/packing-bench.svg";
import "../camera/camera.css";

export type CameraBoardState =
  | "live"
  | "stepped-down"
  | "quota-exhausted"
  | "source-lost"
  | "switched-off"
  | "stop-all"
  | "several-sources"
  | "zone-and-card";

const VIEWS: Record<Exclude<CameraBoardState, "several-sources" | "zone-and-card">, CameraView> = {
  live: live as CameraView,
  "stepped-down": steppedDown as CameraView,
  "quota-exhausted": quotaExhausted as CameraView,
  "source-lost": sourceLost as CameraView,
  "switched-off": switchedOff as CameraView,
  "stop-all": stopAll as CameraView,
};

const DRAWINGS: Record<string, string> = {
  "line-2-live": line2Live,
  "line-2-down": line2Down,
  "line-2-paused": line2Paused,
  "line-2-later": line2Later,
  "line-3": line3,
  "packing-bench": packingBench,
};

/** The drawing an image route names: a frame by its digest, or the held line stop's frame. */
export async function cameraPicture(href: string): Promise<Blob> {
  const last = decodeURIComponent(href.split("?")[0].split("/").pop() ?? "");
  const name = href.includes("/evidence/") ? "line-2-live" : (pictures as Record<string, string>)[last];
  const url = name ? DRAWINGS[name] : undefined;
  if (!url) throw new ApiError(404, "no such frame", "surface.not_found");
  return (await fetch(url)).blob();
}

function viewOf(state: CameraBoardState): CameraView {
  return state === "several-sources" || state === "zone-and-card" ? VIEWS.live : VIEWS[state];
}

/** The camera routes over the boards' views: every act answers with a board's state. */
export class CameraFixtureClient implements CameraClient {
  constructor(private state: CameraBoardState) {}

  async cameras(): Promise<CamerasView> {
    return several as CamerasView;
  }

  async camera(id: string): Promise<CameraView> {
    if (id !== "line-2") throw new ApiError(404, "Only the line 2 camera is in this example", "surface.not_found");
    return viewOf(this.state);
  }

  async switchTo(id: string, on: boolean): Promise<CameraView> {
    this.state = on ? "live" : "switched-off";
    return this.camera(id);
  }

  async stop(id: string): Promise<CameraView> {
    return this.camera(id);
  }

  async retry(id: string): Promise<CameraView> {
    return this.camera(id);
  }

  async saveZone(_id: string, _zone: string, change: ZoneChange): Promise<ZoneSaved> {
    const zone = viewOf(this.state).zones[0];
    return {
      zone: { ...zone, corners: change.corners, cornersText: change.corners.map(Math.round).join(", ") },
      text: "Saved. The seal tear watch uses it from the next frame.",
    };
  }

  async freeze(): Promise<StageView> {
    return later as StageView;
  }

  async raiseCard(_id: string, card: RaiseCardBody): Promise<CardRaised> {
    return {
      digest: card.frame,
      shortDigest: `${card.frame.slice(0, 13)}…${card.frame.slice(-6)}`,
      regions: 5,
      keptUntil: "Kept as evidence until 18 Dec",
      href: `/ui/vision/s-line/vision-${card.frame.slice(7)}.png`,
      outcome: { type: "sent", sessionId: "s-line", text: "Sent" } as unknown as CommandOutcome,
      text: "Raised to the Inspector. This one frame is kept as evidence until 18 Dec.",
    };
  }

  async decide(_command: Command, _origin: Origin): Promise<CommandOutcome> {
    throw new ApiError(403, "Decide on the card in its thread in this example");
  }
}

const INSPECTOR = { name: "inspector", displayName: "Inspector", avatar: { monogram: "I", color: "#7C3AED" } };

/** The web home with the live camera surface open on `state`. */
export class CameraHomeFixture extends FixtureClient {
  constructor(private readonly state: CameraBoardState) {
    super();
  }

  override rows(): AgentRow[] {
    return [
      { agent: agents.chief, status: "working", statusText: "Working", sessionId: "s-chief" },
      { agent: INSPECTOR, status: "asleep", statusText: "Asleep until a defect", sessionId: "s-line" },
      { agent: agents.research, status: "done", statusText: "Done", sessionId: "s-research" },
    ];
  }

  override async status(session?: string): Promise<StatusLine> {
    void session;
    const line = homeStatus(false);
    return { ...line, mode: line.mode ? { ...line.mode, status: "Work · asks first" } : line.mode, decisions: 0 };
  }

  override async decisions() {
    return [];
  }

  async image(href: string): Promise<Blob> {
    return cameraPicture(href);
  }

  cameraClient(): CameraClient {
    return new CameraFixtureClient(this.state);
  }
}

/** Where the state opens, and what is open on it. */
export function cameraPlaceOf(state: CameraBoardState): { place: CameraPlace; opened?: Opened } {
  if (state === "several-sources") return { place: {} };
  return { place: { camera: "line-2" } };
}

/**
 * `camera-zone-and-card-web`: the two panels as the board draws them, one above the other —
 * editing the inspection window with its old outline, and a card raised from the frozen frame.
 * In the home each opens over the camera.
 */
export function CameraActionsBoard() {
  const view = VIEWS.live;
  return (
    <ImageLoaderContext.Provider value={cameraPicture}>
      <div className="cam-board">
        <p className="cam-board-cap">
          <b>1</b>
          <span>Draw or edit a zone</span>
        </p>
        <ZoneEditor
          view={view}
          zone={view.zones[0]}
          initialCorners={[700, 200, 1600, 880]}
          onSave={async () => ({ zone: view.zones[0], text: "Saved." })}
          onCancel={() => undefined}
        />
        <p className="cam-board-cap">
          <b>2</b>
          <span>Raise a card from the current frame</span>
        </p>
        <RaiseCard
          frozen={later as StageView}
          name={view.name}
          agent={view.agent.agent}
          initialNote="The seal on T47 looks lifted at the edge, but nothing fired."
          onRaise={async () => {
            throw new Error("fixture");
          }}
          onCancel={() => undefined}
        />
      </div>
    </ImageLoaderContext.Provider>
  );
}

/** The phone with the live camera surface open on `state` (board choice C14). */
export function cameraPhoneFixture(state: CameraBoardState): { client: FixturePhoneClient; initial: PhoneScreen } {
  const client = new FixturePhoneClient(true);
  client.cards = [];
  const cameras = new CameraFixtureClient(state);
  Object.assign(client, { image: cameraPicture, cameraClient: () => cameras });
  return { client, initial: { screen: "camera", place: cameraPlaceOf(state).place } };
}
