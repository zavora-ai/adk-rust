// Approve with the image (spec 3.7 §8.2) over Document desk's identity-card call, for
// `npm run dev` at /ui/fixture.html?screen=vision&vision=<state> (and `screen=visionPhone`) and
// the screenshots beside the signed-off boards. The pending card is the one the engine drew for
// `examples/vision_home.rs`; the other states change what the engine changes. Never part of the
// built page.
import type {
  AgentRow,
  CardMedia,
  Command,
  CommandOutcome,
  DecisionCard,
  EvidenceImage,
  FeedEntry,
  Origin,
  StatusLine,
} from "../generated/api";
import type { FeedMessage } from "../client";
import { ApiError } from "../client";
import type { PhoneScreen } from "../phone/route";
import { agents, status as homeStatus, FixtureClient } from "./fixture";
import { FixturePhoneClient } from "./phone";
import cardEgress from "./vision/card-egress.png";
import cardChanged from "./vision/card-changed.png";
import cropHeader from "./vision/crop-header.png";
import cropDates from "./vision/crop-dates.png";
import cropNumber from "./vision/crop-number.png";
import contactCovered from "./vision/contact-covered.png";

export type VisionState = "pending" | "parked" | "purged" | "several" | "allRedacted" | "decided" | "changed";

const DESK = { name: "desk", displayName: "Document desk", avatar: { monogram: "D", color: "#2563EB" } };
const INSPECTOR = { name: "inspector", displayName: "Inspector", avatar: { monogram: "I", color: "#7C3AED" } };

/** The images the fixture serves, by the route a card names. */
const IMAGES: Record<string, string> = {};

function at(hh: number, mm: number, dayOffset = 0): string {
  const date = new Date();
  date.setDate(date.getDate() + dayOffset);
  date.setHours(hh, mm, 0, 0);
  return date.toISOString();
}

function day(offset: number): string {
  const date = new Date();
  date.setDate(date.getDate() + offset);
  return date.toLocaleDateString("en-GB", { day: "numeric", month: "short" });
}

function image(id: string, index: number, url: string, fields: Partial<EvidenceImage> & Pick<EvidenceImage, "digest" | "width" | "height">): EvidenceImage {
  const href = `/api/surface/decisions/${id}/evidence/${index}`;
  IMAGES[href] = url;
  const hex = fields.digest.slice(7);
  return {
    index,
    mimeType: "image/png",
    sizeText: `${fields.width} × ${fields.height} PNG`,
    digestShort: `sha256:${hex.slice(0, 6)}…${hex.slice(-6)}`,
    href,
    regions: [],
    coveredAll: false,
    coveredText: "nothing covered",
    ...fields,
  };
}

const EGRESS = "sha256:6b644f480a4058281fbdb2f2e67821a69f702ea65f7c70552289a6add1f31c7c";

/** The identity card's egress copy, as the gate drew it: the portrait and personal text covered. */
function cardImage(id: string): EvidenceImage {
  return image(id, 0, cardEgress, {
    digest: EGRESS,
    width: 600,
    height: 380,
    caption: "to api.anthropic.com as document_crop · 1 face · the whole person is covered, 6 personal details covered",
    coveredText: "1 face, 6 personal text",
    regions: [
      { class: "face", tag: "face · whole person", shortTag: "face", x0: 6, y0: 72, x1: 190, y1: 304 },
      { class: "personalText", tag: "personal text", shortTag: "text", x0: 266, y0: 78, x1: 306, y1: 105 },
      { class: "personalText", tag: "personal text", shortTag: "text", x0: 306, y0: 78, x1: 378, y1: 105 },
      { class: "personalText", tag: "personal text", shortTag: "text", x0: 379, y0: 78, x1: 424, y1: 105 },
      { class: "personalText", tag: "personal text", shortTag: "text", x0: 264, y0: 122, x1: 483, y1: 149 },
      { class: "personalText", tag: "personal text", shortTag: "text", x0: 270, y0: 166, x1: 399, y1: 193 },
      { class: "personalText", tag: "personal text", shortTag: "text", x0: 266, y0: 210, x1: 384, y1: 237 },
    ],
  });
}

function media(id: string, overrides: Partial<CardMedia> = {}): CardMedia {
  return {
    title: "The exact image that would leave",
    images: [cardImage(id)],
    goesTo: { provider: "Claude", model: "claude-sonnet-5-5", host: "api.anthropic.com" },
    what: "Document crop",
    covered: [
      { class: "face", count: 1, text: "1 face" },
      { class: "personalText", count: 6, text: "6 personal text" },
    ],
    coveredAll: false,
    costText: "Up to $0.0070",
    costDetail: "308 image tokens",
    cutFrom: "id-card.png",
    cutNote: "nothing resized",
    keptText: `Kept as evidence until ${day(30)}`,
    canChange: true,
    fixedLine: "Images leave this computer only after faces and personal details are covered. Nothing on this card can uncover them.",
    ...overrides,
  };
}

const CALL = {
  tool: "vision_describe",
  args: { image: "id-card.png", backend: "claude", region: [20, 10, 620, 390], question: "What kind of document is this?" },
  text: 'vision_describe(image: "id-card.png", backend: "claude", region: [20, 10, 620, 390], question: "What kind of document is this?")',
  truncated: false,
};

function pending(id = "dec-7c41e0"): DecisionCard {
  return {
    id,
    version: 1,
    sessionId: "s-desk",
    agent: DESK,
    waitingSince: at(10, 14),
    sinceText: "since 10:14",
    status: "waiting",
    statusText: "Waiting for you",
    heading: "Send this image to Claude",
    call: CALL,
    timeline: [
      { text: "Asked at 10:14", current: false },
      { text: "Waiting for you", current: true },
    ],
    allowed: {
      options: [
        { answer: "allowOnce", label: "Allow once", needsPresence: false, needsExpanded: false },
        { answer: "edit", label: "Edit", needsPresence: false, needsExpanded: false },
        { answer: "deny", label: "Deny", needsPresence: false, needsExpanded: false },
      ],
      openInHome: false,
    },
    risk: "medium",
    riskText: "Medium risk",
    reversibility: "cantBeUnsent",
    reversibilityText: "Can't be unsent",
    money: false,
    alsoShown: "Also on your phone",
    media: media(id),
  };
}

const DENY_ONLY = { options: [{ answer: "deny" as const, label: "Deny", needsPresence: false, needsExpanded: false }], openInHome: false };

/** The card of each state on the boards. */
export function visionCard(state: VisionState): DecisionCard {
  const card = pending();
  switch (state) {
    case "pending":
      return card;
    case "parked":
      return {
        ...card,
        timeline: [
          { text: "Asked at 10:14", current: false },
          { text: "Waited 10 minutes; the run ended at 10:24", current: false },
          { text: "Waiting for you", current: true },
        ],
        parked: {
          at: at(10, 24),
          atText: "10:24",
          text: "Document desk stopped waiting at 10:24 and finished its run without this. Your answer is kept and used when Document desk picks this thread up again.",
        },
      };
    case "purged": {
      const gone = cardImage(card.id);
      return {
        ...card,
        sinceText: `since ${day(-30)}`,
        timeline: [
          { text: `Asked at 10:14`, current: false },
          { text: `Image deleted on ${day(0)}`, current: false },
          { text: "Waiting for you", current: true },
        ],
        allowed: DENY_ONLY,
        media: media(card.id, {
          title: "The image that would have left",
          images: [
            {
              ...gone,
              href: null,
              purged: {
                title: `This image was deleted on ${day(0)}`,
                text: "Evidence is kept for 30 days. What it was, where it was going and what was covered stay on record.",
                day: day(0),
                short: `Image deleted ${day(0)}`,
              },
            },
          ],
          keptText: null,
          canChange: false,
          notice: {
            kind: "purged",
            title: "Nothing to send.",
            text: "The image this decision is about is gone, so it can't be allowed. Deny it, and ask Document desk again if you still need the answer.",
          },
        }),
      };
    }
    case "several": {
      const id = "dec-91d04c";
      return {
        ...pending(id),
        heading: "Send 3 crops of id-card.png to Claude",
        sinceText: "since 10:31",
        call: {
          ...CALL,
          args: { images: ["sha256:4f1a…", "sha256:9c0e…", "sha256:77d2…"], backend: "claude", question: "Read the value in each crop." },
          text: 'vision_describe(images: ["sha256:4f1a…", "sha256:9c0e…", "sha256:77d2…"], backend: "claude", question: "Read the value in each crop.")',
        },
        media: media(id, {
          title: "The 3 images that would leave",
          what: "3 document crops",
          whatNote: "never the page",
          images: [
            image(id, 0, cropHeader, { digest: "sha256:4f1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8", width: 300, height: 64, field: "Country", why: "Faint print" }),
            image(id, 1, cropDates, { digest: "sha256:9c0e1d2c3b4a59687766554433221100ffeeddccbbaa99887766554433221109", width: 240, height: 40, field: "Issued and expires", why: "Small type" }),
            image(id, 2, cropNumber, {
              digest: "sha256:77d2e1f0a9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a1b0c9d8e7f6a5b4c3d2",
              width: 200,
              height: 40,
              field: "ID No",
              why: "A field your schema marks personal",
              coveredText: "1 field your schema marks personal",
              regions: [{ class: "schemaField", tag: "schema field", shortTag: "schema", x0: 52, y0: 4, x1: 170, y1: 32 }],
            }),
          ],
          covered: [{ class: "schemaField", count: 1, text: "1 field your schema marks personal", note: "in image 3" }],
          costText: "Up to $0.0063",
          costDetail: "121 image tokens",
          cutFrom: "id-card.png, page 1",
          cutNote: null,
          canChange: false,
        }),
      };
    }
    case "allRedacted": {
      const id = "dec-3be7a1";
      return {
        ...pending(id),
        heading: "Send a crop of the contact fields to Claude",
        sinceText: "since 10:33",
        reversibilityText: "Can't be unsent",
        call: {
          ...CALL,
          args: { image: "id-card.png", backend: "claude", region: [230, 130, 652, 290], question: "Read the email and phone." },
          text: 'vision_describe(image: "id-card.png", backend: "claude", region: [230, 130, 652, 290], question: "Read the email and phone.")',
        },
        timeline: [
          { text: "Asked at 10:33", current: false },
          { text: "Waiting for you", current: true },
        ],
        allowed: DENY_ONLY,
        media: media(id, {
          images: [
            image(id, 0, contactCovered, {
              digest: "sha256:9cb6f7a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a642f41",
              width: 422,
              height: 160,
              coveredAll: true,
              coveredText: "2 fields your schema marks personal",
              regions: [
                { class: "schemaField", tag: "schema field · email", shortTag: "schema · email", x0: 0, y0: 0, x1: 422, y1: 64 },
                { class: "schemaField", tag: "schema field · phone", shortTag: "schema · phone", x0: 0, y0: 64, x1: 422, y1: 160 },
              ],
            }),
          ],
          covered: [{ class: "schemaField", count: 2, text: "2 fields your schema marks personal" }],
          coveredAll: true,
          coveredAllText: "100% of the image",
          costText: "Up to $0.0063 if sent",
          costDetail: null,
          canChange: false,
          notice: {
            kind: "allCovered",
            title: "Everything is covered.",
            text: "Claude would receive a black rectangle, so there is nothing to allow. Deny it; covering can't be undone here.",
          },
        }),
      };
    }
    case "decided":
    case "changed": {
      const changed = state === "changed";
      const sent = changed
        ? image(card.id, 0, cardChanged, { ...cardImage(card.id), href: "", digest: "sha256:72caa6f0e1d2c3b4a5968778695a4b3c2d1e0f9a8b7c6d5e4f3a2b1c0d9ab9531", height: 266 })
        : cardImage(card.id);
      return {
        ...card,
        status: "decided",
        statusText: "Decided",
        allowed: { options: [], openInHome: false },
        timeline: [
          { text: "Asked at 10:14", current: false },
          { text: changed ? "You said yes in the home app at 10:17" : "You said yes in the home app at 10:16", current: true },
        ],
        decided: {
          answer: "yes",
          at: at(10, changed ? 17 : 16),
          atText: changed ? "10:17" : "10:16",
          by: "You",
          byYou: true,
          text: changed ? "You said yes in the home app at 10:17" : "You said yes in the home app at 10:16",
          via: { surface: "web", origin: { kind: "pointer" } },
          scopeText: "done once",
        },
        media: media(card.id, {
          title: "The image that left",
          images: [sent],
          costText: "$0.0024",
          costDetail: "308 image tokens, 63 answer tokens",
          keptText: `As evidence until ${day(30)}`,
          canChange: false,
          replaces: changed ? EGRESS : null,
        }),
      };
    }
  }
}

function deskThread(state: VisionState): FeedEntry[] {
  const id = visionCard(state).id;
  return [
    { seq: 1, at: at(10, 12), atText: "10:12", items: [{ kind: "message", from: "you", author: "You", text: "What kind of document is id-card.png?" }] },
    {
      seq: 2,
      at: at(10, 13),
      atText: "10:13",
      items: [
        {
          kind: "message",
          from: "agent",
          author: "Document desk",
          text: "I read id-card.png here on your computer. It looks like an identity card, but I can't tell which kind. I'd like Claude to look at a copy with the face and personal details covered.",
        },
      ],
    },
    { seq: 3, at: at(10, 14), atText: "10:14", items: [{ kind: "decision", tool: "vision_describe", text: "Waiting for you", decisionId: id }] },
  ];
}

/** The web home over Document desk's call, in one of the boards' states. */
export class VisionHomeFixture extends FixtureClient {
  private card: DecisionCard;

  constructor(private readonly state: VisionState) {
    super();
    this.card = visionCard(state);
  }

  override rows(): AgentRow[] {
    const waiting = this.card.status === "waiting";
    return [
      { agent: agents.chief, status: "working", statusText: "Working", sessionId: "s-chief" },
      { agent: DESK, status: waiting ? "waitingForYou" : "done", statusText: waiting ? "Waiting for you" : "Done", sessionId: "s-desk" },
      { agent: INSPECTOR, status: "working", statusText: "Watching line 2", sessionId: "s-inspector" },
      { agent: agents.finance, status: "done", statusText: "Done", sessionId: "s-finance" },
      { agent: agents.research, status: "asleep", statusText: "Asleep until 07:00", sessionId: "s-research" },
    ];
  }

  override async status(session?: string): Promise<StatusLine> {
    void session;
    return { ...homeStatus(false), decisions: this.card.status === "waiting" ? 1 : 0 };
  }

  override async briefing() {
    const away = await super.briefing();
    return { ...away, lines: [], waiting: 0 };
  }

  override async decisions() {
    return this.card.status === "waiting" ? [this.card] : [];
  }

  override async decision(id: string) {
    if (id !== this.card.id) throw new ApiError(404, `decision '${id}' not found`, "surface.not_found");
    return this.card;
  }

  async image(href: string): Promise<Blob> {
    const url = IMAGES[href];
    if (!url) throw new ApiError(404, "no such image", "surface.not_found");
    return (await fetch(url)).blob();
  }

  override async command(command: Command, origin: Origin): Promise<CommandOutcome> {
    if (command.type === "changeImage") {
      const changed = { from: command.from, to: "sha256:72caa6f0e1d2c3b4a5968778695a4b3c2d1e0f9a8b7c6d5e4f3a2b1c0d9ab9531", width: command.change.crop ? command.change.crop[2] - command.change.crop[0] : 600, height: command.change.crop ? command.change.crop[3] - command.change.crop[1] : 380 };
      if (command.preview) return { type: "imagePreview", changed };
      return { type: "heldAgain", changed, card: visionCard("changed") };
    }
    if (command.type === "decide") {
      const decided = visionCard(command.answer === "deny" ? "decided" : "decided");
      this.card = decided;
      return { type: "answered", card: decided };
    }
    return super.command(command, origin);
  }

  override async *feed(session: string, after: number | undefined, signal: AbortSignal): AsyncIterable<FeedMessage> {
    if (session !== "s-desk") {
      yield* super.feed(session, after, signal);
      return;
    }
    for (const entry of deskThread(this.state)) if (entry.seq > (after ?? 0)) yield { type: "entry", entry };
    yield { type: "decisions", cards: this.card.status === "waiting" ? [this.card] : [] };
    while (!signal.aborted) await new Promise((resolve) => setTimeout(resolve, 200));
  }
}

/** The phone over the same call. */
export function visionPhoneFixture(state: VisionState): { client: FixturePhoneClient; initial: PhoneScreen } {
  const client = new FixturePhoneClient(true);
  const card = visionCard(state);
  client.cards = [card];
  Object.assign(client, {
    image: async (href: string) => {
      const url = IMAGES[href];
      if (!url) throw new ApiError(404, "no such image", "surface.not_found");
      return (await fetch(url)).blob();
    },
  });
  return { client, initial: { screen: "decide", id: card.id } };
}
