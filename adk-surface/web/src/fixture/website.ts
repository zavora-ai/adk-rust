/**
 * The website's views over a bakery's site (spec 3.5 §8.2), held in memory, for component tests
 * and `/ui/fixture.html?screen=website`. It answers as the desk does, in its words, for the
 * decisions the tests make. Never part of the built page.
 */
import type {
  ClaimedView,
  CharterCommand,
  CharterResponse,
  DraftView,
  EditorCommand,
  EditorHome,
  EditorOutcome,
  FieldSpec,
  JobLine,
  OnboardingCommand,
  OnboardingOutcome,
  OnboardingView,
  ReviewCommand,
  ReviewPage,
  ReviewView,
  SetupView,
  SiteCommand,
  SiteOutcome,
  SiteViewModel,
  UploadRequest,
  WebsiteHome,
} from "../generated/website";
import { ApiError } from "../client";
import type { WebsiteClient } from "../website/client";

/** The code the fixture's fresh site prints. */
export const SETUP_CODE = "K7Q4-9MXC";

const NAME = "Juniper Bakery";
const ORIGIN = "https://juniper-bakery.example";

function job(id: string, title: string, level: string, extra: Partial<JobLine> = {}): JobLine {
  return {
    id,
    title,
    area: "Content",
    level,
    since: "2026-10-18T09:00:00Z",
    agent: "Editor",
    canLower: true,
    canPromote: false,
    offered: false,
    ...extra,
  };
}

function site(): SiteViewModel {
  return {
    name: NAME,
    origin: ORIGIN,
    launched: true,
    owner: true,
    status: { state: { text: "Up", tone: "good" }, since: "2026-10-18T09:00:00Z" },
    alarms: [
      {
        id: "alarm-1",
        text: "The order form failed twice in an hour",
        raisedAt: "2026-10-25T07:40:00Z",
        urgency: { text: "Within the hour", tone: "waiting" },
        canAcknowledge: true,
      },
    ],
    releases: [
      {
        seq: 3,
        at: "2026-10-24T16:00:00Z",
        source: "Weekly menu",
        otherFiles: 2,
        pages: [
          { path: "/menu", change: { text: "Changed", tone: "info" } },
          { path: "/menu/autumn", change: { text: "New page", tone: "good" } },
        ],
      },
      { seq: 1, at: "2026-10-18T09:00:00Z", source: "Release 1", otherFiles: 0, pages: [{ path: "/", change: { text: "New page", tone: "good" } }] },
    ],
    kpis: [
      {
        id: "orders",
        name: "Orders from the site",
        phase: "Days 1 to 30",
        thisWeek: "14",
        lastWeek: "9",
        baseline: "6",
        target: "20",
        trend: { text: "Better", tone: "good" },
      },
    ],
    jobs: [
      job("menu", "Keep the menu current", "Suggest", { canPromote: true, offered: true, next: "Act and tell", record: "12 of 12 approved unchanged" }),
      job("hours", "Keep opening hours right", "Act and tell"),
      job("posts", "Write a weekly post", "Suggest"),
      job("reviews", "Answer reviews", "Suggest", { area: "Customers", agent: "Concierge" }),
      job("seo", "Keep search listings right", "Suggest", { area: "Growth", agent: "Analyst" }),
      job("orders", "Confirm orders", "Ask first", { area: "Commerce", agent: "Clerk" }),
      job("stock", "Mark sold-out items", "Ask first", { area: "Commerce", agent: "Clerk" }),
      job("probe", "Check the site every hour", "Act and tell", { area: "Operations", agent: "Steward", canLower: false }),
    ],
    signals: {
      open: 2,
      lines: [
        { text: "Asked whether the sourdough is vegan", count: 3, lastAt: "2026-10-24T18:20:00Z" },
        { text: "An agent asked for the opening hours", count: 1, lastAt: "2026-10-25T06:10:00Z" },
      ],
    },
    held: {
      items: [{ id: "held-1", batch: 4, heldAt: "2026-10-25T05:00:00Z", title: "Pumpkin loaf", reason: "The price rose by 40%, above the 25% the charter allows" }],
      deliveries: [
        {
          delivery: 7,
          feed: "Supplier stock",
          receivedAt: "2026-10-25T04:00:00Z",
          breaks: ["Removes 12 of 30 items at once"],
          would: ["Marks the rye loaf sold out"],
        },
      ],
      batches: [{ batch: 3, closedAt: "2026-10-24T05:00:00Z", text: "8 updates published" }],
    },
    wallet: {
      balance: "$182.40",
      spent: "$37.60",
      budget: "$120.00",
      categories: [{ name: "Models", spent: "$21.10", cap: "$60.00", percent: 35 }],
      held: [],
    },
  };
}

function fields(): FieldSpec[] {
  return [
    { id: "title", label: "Title", kind: "text", required: true, value: "" },
    { id: "summary", label: "Summary", kind: "longText", required: false, hint: "One or two sentences for listings.", value: "" },
  ];
}

function editorHome(): EditorHome {
  return {
    canEdit: true,
    kinds: [
      { kind: "post", title: "Post", description: "News and stories, with a date.", fields: fields() },
      { kind: "menu_item", title: "Menu item", description: "One thing on the menu, with its price.", fields: fields() },
    ],
    languages: [{ value: "en", label: "English" }],
    drafts: [],
    items: [
      {
        id: "item-1",
        kind: "Post",
        title: "Our autumn menu",
        slug: "autumn-menu",
        language: "en",
        live: true,
        state: { text: "Live", tone: "good" },
      },
    ],
  };
}

function interview(): OnboardingView {
  const later = (id: string, number: number, title: string) => ({ id, number, title, state: "later" as const });
  return {
    siteName: NAME,
    current: "interview",
    stages: [
      { id: "start", number: 1, title: "Start", state: "done" },
      { id: "discover", number: 2, title: "Discover", state: "done" },
      { id: "protect", number: 3, title: "Protect", state: "done" },
      { id: "interview", number: 4, title: "Interview", state: "current" },
      later("charter", 5, "Charter"),
      later("build", 6, "Build"),
      later("launch_gate", 7, "Launch check"),
      later("connect_and_deploy", 8, "Connect and deploy"),
      later("run", 9, "Run"),
    ],
    start: { started: true, from: "From your profiles and listings", phone: false },
    discovery: {
      ran: true,
      source: "https://maps.example/juniper-bakery",
      found: [{ label: "Pages read", value: "2" }],
      facts: [
        {
          id: "fact-hours",
          text: "Opening hours: Tuesday to Sunday, 7:00 to 15:00",
          source: "https://maps.example/juniper-bakery",
          status: { text: "A guess", tone: "waiting" },
          open: true,
        },
      ],
      kind: { proposed: "business", confirmed: "business", options: [{ value: "business", label: "Business" }], reasons: ["it lists opening hours"] },
      decisions: [],
    },
    interview: {
      done: false,
      forms: [
        {
          id: "contact",
          question: "How do customers reach you?",
          answered: false,
          fromDiscovery: false,
          fields: [{ id: "email", label: "Public email", kind: "email", required: true, value: "" }],
        },
      ],
      preview: {
        siteName: NAME,
        about: "A neighbourhood bakery.",
        facts: 1,
        pages: [{ title: "About", summary: "Who we are and when we are open." }],
        published: true,
        rebuiltAt: "2026-10-25T08:00:00Z",
      },
    },
    connections: { notice: "We never ask for your passwords.", cards: [] },
    design: { options: [], held: [] },
    deploy: { canGoLive: false, cost: [], shape: "Single server or container" },
  };
}

function review(): ReviewView {
  return {
    week: "2026-W43",
    title: "Week of 20 October",
    summary: "14 orders, up from 9; the menu job earned more freedom; one alarm waited.",
    markdown: "# Week of 20 October\n\n14 orders from the site.",
    headline: [
      { label: "Orders", value: "14" },
      { label: "Spent", value: "$37.60" },
    ],
    kpis: site().kpis,
    work: [{ agent: "Editor", items: ["Updated the menu twice"] }],
    money: {
      spend: "$9.40",
      month: "$37.60",
      categories: [{ label: "Models", value: "$21.10" }],
      results: [{ label: "Orders taken", value: "$412.00" }],
      within: { text: "Within budget", tone: "good" },
    },
    experiments: [],
    incidents: [],
    leads: ["A café asked about wholesale bread"],
    promotions: [{ job: "menu", title: "Keep the menu current", from: "Suggest", to: "Act and tell", record: "12 of 12 approved unchanged" }],
    demotions: [],
    alarms: ["The order form failed twice in an hour"],
    questions: ["Should the menu show allergens on every item?"],
    updates: [
      { label: "Updates", value: "46 (40 live, 6 in releases)" },
      { label: "Feed deliveries", value: "7, 1 held whole" },
    ],
  };
}

function draftOf(id: string, kind: string, title: string): DraftView {
  return {
    id,
    kind,
    kindTitle: title,
    author: "Amara",
    slug: "",
    language: "en",
    body: "",
    fields: fields(),
    media: [],
    checks: [],
    preview: [],
    schedule: {},
    status: { text: "Draft", tone: "neutral" },
  };
}

/** Every call the fixture took, for tests. */
export interface WebsiteTaken {
  claims: Array<{ code: string; device: string }>;
  onboarding: OnboardingCommand[];
  site: SiteCommand[];
  review: ReviewCommand[];
  reviewWeeks: Array<string | undefined>;
  editor: EditorCommand[];
  uploads: UploadRequest[];
}

export class WebsiteFixture implements WebsiteClient {
  readonly taken: WebsiteTaken = { claims: [], onboarding: [], site: [], review: [], reviewWeeks: [], editor: [], uploads: [] };
  needsSetup: boolean;
  launched: boolean;
  view: SiteViewModel = site();
  boarding: OnboardingView = interview();
  page: ReviewPage = {
    canCompose: true,
    channel: "whatsapp",
    nextDue: "2026-11-01T08:00:00Z",
    current: review(),
    reviews: [
      { week: "2026-W43", title: "Week of 20 October" },
      { week: "2026-W42", title: "Week of 13 October", sent: "Sent to WhatsApp" },
    ],
  };
  home_: EditorHome = editorHome();
  drafts = new Map<string, DraftView>();

  /** A fresh site waits for its setup code; otherwise the site is a week past launch. */
  constructor({ fresh = false }: { fresh?: boolean } = {}) {
    this.needsSetup = fresh;
    this.launched = !fresh;
  }

  async home(): Promise<WebsiteHome> {
    return { name: NAME, origin: ORIGIN, launched: this.launched, needsSetup: this.needsSetup, roles: this.needsSetup ? [] : ["owner"] };
  }

  async setup(): Promise<SetupView> {
    return {
      siteName: NAME,
      needed: this.needsSetup,
      paired: false,
      whereCode: "The code is in the site's logs, on the line that starts with “setup code”.",
      holding: "Juniper Bakery is opening soon.",
    };
  }

  async claim(code: string, device: string): Promise<ClaimedView> {
    this.taken.claims.push({ code, device });
    if (code !== SETUP_CODE) throw new ApiError(403, "That setup code is not right. Check the site's logs.", "website.setup.wrong_code");
    this.needsSetup = false;
    return { device, token: "fixture-device-token", expiresAt: "2026-11-25T00:00:00Z" };
  }

  async onboarding(): Promise<OnboardingView> {
    return structuredClone(this.boarding);
  }

  async onboard(command: OnboardingCommand): Promise<OnboardingOutcome> {
    this.taken.onboarding.push(command);
    let note: string | undefined;
    if (command.type === "fact") {
      const fact = this.boarding.discovery.facts.find((line) => line.id === command.id);
      if (fact && command.answer !== "forget") {
        fact.text = command.answer === "fix" && command.text ? command.text : fact.text;
        fact.status = { text: "You said this", tone: "good" };
        fact.open = false;
      } else {
        this.boarding.discovery.facts = this.boarding.discovery.facts.filter((line) => line.id !== command.id);
      }
    } else if (command.type === "answer") {
      const form = this.boarding.interview.forms.find((line) => line.id === command.form);
      if (form) {
        form.answered = true;
        form.fields = form.fields.map((field) => ({ ...field, value: command.values[field.id] ?? field.value }));
      }
      const preview = this.boarding.interview.preview;
      if (preview) preview.facts += 1;
      note = "The preview is rebuilt with your answer.";
    }
    return { view: structuredClone(this.boarding), note };
  }

  async signCharter(_command: CharterCommand): Promise<CharterResponse> {
    return { charter: this.boarding.charter ?? null };
  }

  async site(): Promise<SiteViewModel> {
    return structuredClone(this.view);
  }

  async decideSite(command: SiteCommand): Promise<SiteOutcome> {
    this.taken.site.push(command);
    let note: string | undefined;
    switch (command.type) {
      case "promote": {
        const line = this.view.jobs.find((one) => one.id === command.job);
        if (line?.next) {
          note = `${line.title} now does more: ${line.next}.`;
          Object.assign(line, { level: line.next, next: null, canPromote: false, offered: false });
        }
        if (this.page.current) this.page.current.promotions = this.page.current.promotions.filter((offer) => offer.job !== command.job);
        break;
      }
      case "lower": {
        const line = this.view.jobs.find((one) => one.id === command.job);
        if (line) note = `${line.title} is lowered.`;
        break;
      }
      case "item":
        this.view.held.items = this.view.held.items.filter((item) => item.id !== command.id);
        note = command.decision === "release" ? "Published." : "Discarded.";
        break;
      case "delivery":
        this.view.held.deliveries = this.view.held.deliveries.filter((line) => line.delivery !== command.delivery);
        note = command.decision === "release" ? "Applied whole." : "Discarded whole.";
        break;
      case "acknowledge":
        this.view.alarms = this.view.alarms.filter((alarm) => alarm.id !== command.alarm);
        break;
    }
    return { view: structuredClone(this.view), note };
  }

  async review(week?: string): Promise<ReviewPage> {
    this.taken.reviewWeeks.push(week);
    const page = structuredClone(this.page);
    if (week && page.current && week !== page.current.week) {
      const line = page.reviews.find((one) => one.week === week);
      page.current = { ...page.current, week, title: line?.title ?? week, sent: line?.sent ?? null, promotions: [] };
    }
    return page;
  }

  async decideReview(command: ReviewCommand): Promise<ReviewPage> {
    this.taken.review.push(command);
    if (command.type === "send" && this.page.current) {
      this.page.current.sent = "Sent to WhatsApp just now";
      this.page.reviews = this.page.reviews.map((line) => (line.week === command.week ? { ...line, sent: "Sent to WhatsApp" } : line));
    }
    return structuredClone(this.page);
  }

  async editor(): Promise<EditorHome> {
    return structuredClone(this.home_);
  }

  async draft(id: string): Promise<DraftView> {
    const draft = this.drafts.get(id);
    if (!draft) throw new ApiError(404, "There is no such draft.", "website.editor.not_found");
    return structuredClone(draft);
  }

  async edit(command: EditorCommand): Promise<EditorOutcome> {
    this.taken.editor.push(command);
    let draft: DraftView | undefined;
    let note: string | undefined;
    switch (command.type) {
      case "new": {
        const form = this.home_.kinds.find((kind) => kind.kind === command.kind);
        draft = draftOf(`draft-${this.drafts.size + 1}`, command.kind, form?.title ?? command.kind);
        break;
      }
      case "open":
        draft = { ...draftOf(`draft-${this.drafts.size + 1}`, "post", "Post"), slug: "autumn-menu", changes: command.id };
        break;
      case "save":
        draft = this.drafts.get(command.draft);
        if (draft) {
          Object.assign(draft, { slug: command.slug, language: command.language, body: command.body ?? "", schedule: command.schedule ?? {} });
          draft.fields = draft.fields.map((field) => ({ ...field, value: command.fields[field.id] ?? field.value }));
        }
        note = "Saved.";
        break;
      case "preview":
        draft = this.drafts.get(command.draft);
        if (draft) {
          draft.checkText = "Every check passed.";
          draft.preview = [{ path: `/posts/${draft.slug}`, title: draft.fields[0]?.value ?? "", change: { text: "New page", tone: "good" }, markdown: draft.body }];
        }
        break;
      case "publish":
        draft = this.drafts.get(command.draft);
        if (draft) {
          draft.status = { text: "Published", tone: "good" };
          draft.outcome = "Published in release 4.";
        }
        break;
      case "correct":
      case "withdraw":
        note = command.type === "correct" ? "Corrected: readers see your note." : "Withdrawn.";
        break;
      case "discard":
        this.drafts.delete(command.draft);
        break;
      case "approve":
      case "reject":
      case "confirmAlt":
        draft = this.drafts.get(command.draft);
        break;
    }
    if (draft) this.drafts.set(draft.id, draft);
    this.home_.drafts = [...this.drafts.values()].map((line) => ({
      id: line.id,
      kind: line.kindTitle,
      title: line.fields[0]?.value || "Untitled",
      author: line.author,
      status: line.status,
    }));
    return { home: structuredClone(this.home_), draft: draft ? structuredClone(draft) : null, note };
  }

  async upload(draft: string, file: UploadRequest): Promise<DraftView> {
    this.taken.uploads.push(file);
    const view = this.drafts.get(draft);
    if (!view) throw new ApiError(404, "There is no such draft.", "website.editor.not_found");
    view.media.push({ name: file.name, path: `/media/${file.name}`, contentType: "image/png", alt: file.alt, altConfirmed: false, generated: false });
    return structuredClone(view);
  }
}
