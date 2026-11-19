/**
 * The words of the approve-with-image card (spec 3.7 §8.2, board choices S3–S5, A1–A21) that
 * the page adds around the engine's: the engine's own words come in the card and from
 * `generated/words.ts`.
 */
import * as words from "../generated/words";

export const evidenceLabels = {
  regions: "Regions",
  imageOnly: "Image only",
  tightenTheCrop: "Tighten the crop",
  addARedaction: "Add a redaction",
  openTheSourceImage: words.OPEN_THE_SOURCE_IMAGE,
  goesTo: "Goes to",
  wasGoingTo: "Was going to",
  sentTo: "Sent to",
  whatItIs: "What it is",
  whatTheyAre: "What they are",
  coveredBeforeItLeaves: "Covered before it leaves",
  coveredBeforeTheyLeave: "Covered before they leave",
  estimatedCost: "Estimated cost",
  image: "Image",
  thisImage: "This image",
  cutFrom: "Cut from",
  deleted: "Deleted",
  copyDigest: "Copy the digest",
  copied: "Copied",
  whatLeft: "What left",
  to: "To",
  covered: "Covered",
  cost: "Cost",
  kept: "Kept",
  onRecord: "On record",
  inWhatYourAgentsDid: "In What your agents did",
  originalNeverLeft: "the original never left this computer",
  originalNeverLeftPhone: "The original never left your computer.",
  coveringCantBeUndone: "Covering can't be undone here",
  imageOf: (index: number, count: number) => `${index} of ${count}`,
  swipeForTheOthers: (index: number, count: number) => `Image ${index} of ${count} · swipe for the others`,
  loading: "Loading the image…",
  cantShow: "The image could not be shown here. Its digest is below.",
  replaces: "Replaces",
  // The editor (board choice A8).
  changeTheImage: "Change the image before it leaves",
  undo: "Undo",
  yourChanges: "Your changes",
  noChangesYet: "Nothing changed yet.",
  cutTo: (width: number, height: number, was: string) => `Cut to ${width} × ${height} (was ${was})`,
  boxesCovered: (count: number) => `${count} box${count === 1 ? "" : "es"} you covered`,
  theNewImage: "The new image",
  newDigestPending: "Made when you change something",
  editorHint: "Drag the edges to cut the image smaller. Draw a box to cover more of it. You can cover more, never less.",
  editorNote:
    "The new image is saved and recorded. The call is then held again with it, and only the new image can be sent.",
  useThisImage: "Use this image",
  heldAgain: (to: string, from: string) => `Held again with ${to}, which replaces ${from}. Answer the new card.`,
  cropHandle: (edge: string) => `Move the ${edge} edge`,
  answeredElsewhereUpdates: "If you answer somewhere else first, this screen updates.",
};
