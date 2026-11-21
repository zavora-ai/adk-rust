import type {
  CameraView,
  CamerasView,
  CardRaised,
  Command,
  CommandOutcome,
  Origin,
  RaiseCard,
  StageView,
  ZoneChange,
  ZoneSaved,
} from "../generated/api";

/** One message of a camera's feed (spec 3.7 §8.2): the picture after a frame, or "read it again". */
export type CameraFeedMessage = { type: "frame"; stage: StageView } | { type: "changed" };

/** The live camera surface's routes, as the signed-in person (spec 3.7 §8.2). */
export interface CameraClient {
  cameras(): Promise<CamerasView>;
  camera(id: string): Promise<CameraView>;
  /** The camera's picture after each frame, until `signal` aborts. */
  feed?(id: string, signal: AbortSignal): AsyncIterable<CameraFeedMessage>;
  /** On, or off: off drops every frame on the server from then on. */
  switchTo(id: string, on: boolean): Promise<CameraView>;
  stop(id: string): Promise<CameraView>;
  retry(id: string): Promise<CameraView>;
  saveZone?(id: string, zone: string, change: ZoneChange): Promise<ZoneSaved>;
  /** Holds the frame on screen in the server's memory while the card is written. */
  freeze(id: string, frame: string): Promise<StageView>;
  raiseCard(id: string, card: RaiseCard): Promise<CardRaised>;
  /** A decision shown beside the camera: the held line stop (board choice C13). */
  decide(command: Command, origin: Origin): Promise<CommandOutcome>;
}
