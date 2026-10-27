/**
 * Agent-drawn UI over the example of spec 3.3 §5.1 (spec 3.3 §6.1 `genui`, §7.4 rule 5):
 * Finance's invoice form, its card and table, a refused drawing, the decision a Send asks for,
 * and every state of the concept screens. For `npm run dev` at
 * `/ui/fixture.html?screen=genui&genui=…`, the screenshots beside the spec's images, and the
 * tests. Not part of the built page.
 */
import type { CSSProperties, ReactNode } from "react";
import type {
  CommandOutcome,
  Command,
  DecisionCard,
  DrawnCheck,
  DrawnNode,
  DrawnSurface,
  FeedEntry,
  FormState,
  GenuiCapabilities,
  Origin,
  RefusedDrawing,
  ThreadItem,
} from "../generated/api";
import type { FeedMessage } from "../client";
import { ApiError } from "../client";
import { App } from "../App";
import { DecisionCard as CardView } from "../components/DecisionCard";
import { Developer } from "../developer/Developer";
import { DrawnView } from "../genui/DrawnView";
import { RefusedNotice } from "../genui/Refused";
import { FixtureClient, agents, fixtureSeams } from "./fixture";

function at(hh: number, mm: number): string {
  const date = new Date();
  date.setHours(hh, mm, 0, 0);
  return date.toISOString();
}

function text(key: string, value: string, variant?: string): DrawnNode {
  return { key, component: "Text", text: value, variant };
}

function field(
  key: string,
  label: string,
  input: "shortText" | "number" | "date" | "checkbox",
  value: unknown,
  checks: DrawnCheck[] = [],
): DrawnNode {
  return {
    key: `field-${key}`,
    component: input === "date" ? "DateTimeInput" : input === "checkbox" ? "CheckBox" : "TextField",
    text: label,
    variant: input === "number" ? "number" : undefined,
    field: { path: `/${key}`, input, value, checks },
  };
}

/** Finance's invoice form, version `version`, drawn at `hh:mm`. */
export function invoiceForm(version = 1, hh = 10, mm = 2): DrawnSurface {
  const stamp = `${hh}:${String(mm).padStart(2, "0")}`;
  return {
    surfaceId: "invoice-10",
    version,
    kind: "form",
    catalog: "forms 1.0",
    agent: agents.finance,
    drawnAt: at(hh, mm),
    atText: stamp,
    drawnText: `Drawn by Finance · ${stamp}`,
    changedText: `Finance changed this form at ${stamp}`,
    earlierText: `Earlier version · ${stamp}`,
    title: "Invoice INV-10",
    root: {
      key: "root",
      component: "Card",
      children: [
        {
          key: "body",
          component: "Column",
          children: [
            field("to", "To", "shortText", "ana@example.com", [
              { condition: { call: "required", args: { value: { path: "/to" } } }, message: "Fill this in" },
              { condition: { call: "email", args: { value: { path: "/to" } } }, message: "Enter an email address" },
            ]),
            {
              key: "money",
              component: "Row",
              children: [field("amount", "Amount", "number", 4200), field("due", "Due date", "date", "2026-11-15")],
            },
            field("copy", "Send a copy to me", "checkbox", false),
            {
              key: "actions",
              component: "Row",
              children: [{ key: "action-send", component: "Button", text: "Send", variant: "primary", action: "send" }],
            },
          ],
        },
      ],
    },
    actions: [
      {
        id: "send",
        label: "Send",
        tool: "send_invoice",
        note: "Sending runs send_invoice and asks you first",
        args: [
          { name: "to", path: "/to" },
          { name: "amount", path: "/amount" },
          { name: "invoice", value: "INV-10" },
        ],
      },
    ],
    callId: "c-draw",
  };
}

/** Finance's timesheet card. */
export function timesheetCard(version = 1, hh = 10, mm = 2): DrawnSurface {
  const stamp = `${hh}:${String(mm).padStart(2, "0")}`;
  const row = (index: number, label: string, value: string): DrawnNode => ({
    key: `row-${index}`,
    component: "Row",
    variant: "spaceBetween",
    children: [text(`row-${index}-label`, label, "caption"), text(`row-${index}-value`, value)],
  });
  return {
    ...invoiceForm(version, hh, mm),
    surfaceId: "card-october-timesheet",
    kind: "card",
    title: "October timesheet",
    changedText: `Finance changed this at ${stamp}`,
    actions: [],
    root: {
      key: "root",
      component: "Card",
      children: [
        {
          key: "body",
          component: "Column",
          children: [
            row(0, "Client", "[CLIENT]"),
            row(1, "Period", "1–31 October"),
            row(2, "Hours", "42"),
            row(3, "Total", "[TOTAL]"),
          ],
        },
      ],
    },
  };
}

const TASKS = ["Design review", "Client call", "Invoice template", "Data import", "Client call", "Report draft", "Report review", "Data import", "Design review", "Client call"];

/** Finance's hours table: 42 rows, ten shown. */
export function hoursTable(): DrawnSurface {
  const days = Array.from({ length: 42 }, (_, index) => index);
  return {
    ...invoiceForm(),
    surfaceId: "table-hours-in-october",
    kind: "table",
    title: "Hours in October",
    actions: [],
    root: {
      key: "root",
      component: "Card",
      children: [
        {
          key: "body",
          component: "Column",
          children: [
            {
              key: "header",
              component: "Row",
              children: [text("head-0", "Date", "caption"), text("head-1", "Task", "caption"), text("head-2", "Hours", "caption")],
            },
            {
              key: "rows",
              component: "List",
              showAllText: "Show all 42",
              children: days.map((index) => ({
                key: `row@/rows/${index}`,
                component: "Row",
                children: [
                  text(`cell-0@/rows/${index}`, `2026-10-${String((index % 28) + 1).padStart(2, "0")}`),
                  text(`cell-1@/rows/${index}`, TASKS[index % TASKS.length]),
                  text(`cell-2@/rows/${index}`, String((index % 4) + 1)),
                ],
              })),
            },
          ],
        },
      ],
    },
  };
}

export function refusal(kind: RefusedDrawing["kind"] = "looksLikeDecision"): RefusedDrawing {
  const words = {
    looksLikeDecision: "Finance tried to show something that looks like a decision. It wasn't shown.",
    cannotShow: "Finance tried to show something this home can't show.",
    asksForSecret: "Finance asked for a password in a form. Use Take over for passwords.",
  };
  return {
    agent: agents.finance,
    kind,
    text: words[kind],
    reason: 'catalog forms 1.0 · component c7 Button · label matches reserved word "Allow once" after normalisation',
    component: "c7",
    at: at(10, 2),
    atText: "10:02:41",
    tool: "render_screen",
    callId: "c-refused",
  };
}

/** The decision a Send on the invoice form asks for. */
export function formCard(overrides: Partial<DecisionCard> = {}): DecisionCard {
  return {
    id: "apr_form",
    version: 1,
    sessionId: "s-finance",
    agent: agents.finance,
    waitingSince: at(10, 3),
    sinceText: "since 10:03",
    status: "waiting",
    statusText: "Waiting for you",
    heading: "Send invoice INV-10 to ana@example.com",
    call: {
      tool: "send_invoice",
      args: { to: "ana@example.com", amount: 4200, invoice: "INV-10" },
      text: 'send_invoice(to: "ana@example.com", amount: 4200, invoice: "INV-10")',
      truncated: false,
    },
    timeline: [
      { text: "You pressed Send on a form Finance drew · 10:03", current: false },
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
    detail: { text: "From a form Finance drew" },
    ...overrides,
  };
}

function sentState(status: FormState["status"], text: string, locked = true): FormState {
  return { surfaceId: "invoice-10", version: 1, status, text, locked, decisionId: "apr_form" };
}

/** The thread of `variant`: the form fresh, sent, or with a refused drawing; the card and table. */
function thread(variant: string): FeedEntry[] {
  const entry = (seq: number, hh: number, mm: number, items: ThreadItem[]): FeedEntry => ({
    seq,
    at: at(hh, mm),
    atText: `${hh}:${String(mm).padStart(2, "0")}`,
    items,
  });
  const asked = entry(1, 10, 1, [{ kind: "message", from: "you", author: "You", text: "Prepare the October invoice for ACME" }]);
  if (variant === "display") {
    return [
      entry(1, 10, 1, [{ kind: "message", from: "you", author: "You", text: "How is October going?" }]),
      entry(2, 10, 2, [{ kind: "message", from: "agent", author: "Finance", text: "Here is October so far." }]),
      entry(3, 10, 2, [{ kind: "drawn", surface: timesheetCard() }]),
      entry(4, 10, 2, [{ kind: "drawn", surface: hoursTable() }]),
    ];
  }
  const drawn = [
    asked,
    entry(2, 10, 2, [{ kind: "message", from: "agent", author: "Finance", text: "Here is the invoice. Check the details and press Send when it is right." }]),
    entry(3, 10, 2, [{ kind: "drawn", surface: invoiceForm() }]),
  ];
  if (variant === "refused") return [...drawn, entry(4, 10, 2, [{ kind: "drawnRefused", refusal: refusal() }])];
  if (variant === "sent") {
    return [
      ...drawn,
      entry(4, 10, 3, [
        {
          kind: "drawnSent",
          sent: {
            surfaceId: "invoice-10",
            version: 1,
            actionId: "send",
            tool: "send_invoice",
            callId: "adk-call-requested-1",
            decisionId: "apr_form",
            held: true,
            at: at(10, 3),
            text: "Sent at 10:03",
          },
        },
        { kind: "decision", decisionId: "apr_form", tool: "send_invoice", text: "Waiting for you" },
      ]),
    ];
  }
  return drawn;
}

/** The example's client with Finance's thread drawn as `variant`, and the genui routes. */
export class GenuiFixtureClient extends FixtureClient {
  constructor(private readonly variant: string) {
    super();
    if (variant === "sent") this.hold(formCard());
  }

  override async *feed(session: string, after: number | undefined, signal: AbortSignal): AsyncIterable<FeedMessage> {
    if (session !== "s-finance") {
      yield* super.feed(session, after, signal);
      return;
    }
    for (const entry of thread(this.variant)) if (entry.seq > (after ?? 0)) yield { type: "entry", entry };
    const open = (await this.decisions()).filter((card) => card.sessionId === session);
    yield { type: "decisions", cards: open };
    while (!signal.aborted) await new Promise((resolve) => setTimeout(resolve, 200));
  }

  override async command(command: Command, origin: Origin): Promise<CommandOutcome> {
    if (command.type === "submitDrawn") {
      this.taken.commands.push({ command, origin });
      return { type: "formSent", sessionId: command.sessionId, surfaceId: command.surfaceId, version: command.version, callId: "adk-call-requested-2" };
    }
    return super.command(command, origin);
  }

  override async get<T>(path?: string): Promise<T> {
    if (path?.startsWith("/surface/genui/capabilities")) return capabilities() as T;
    if (path?.startsWith("/surface/genui/sessions/")) {
      const forms = this.variant === "sent" ? [sentState("waiting", "Sent at 10:03 · waiting for you")] : [];
      return { sessionId: "s-finance", forms } as T;
    }
    if (path?.startsWith("/surface/sessions/")) {
      return { sessionId: "s-finance", entries: [{ seq: 4, at: at(10, 2), atText: "10:02", items: refusedLog().map((refusal) => ({ kind: "drawnRefused", refusal })) }] } as T;
    }
    throw new ApiError(404, "not in the fixture");
  }
}

/** The refused drawings of the Protocols tab. */
function refusedLog(): RefusedDrawing[] {
  return [
    refusal(),
    { ...refusal(), atText: "10:02:41", reason: 'catalog forms 1.0 · component c9 Text · text matches reserved word "Approved" (whole label) after normalisation', component: "c9" },
    { ...refusal("cannotShow"), atText: "09:48:10", reason: "catalog forms 1.0 · component c3 Image · not in the catalog", component: "c3" },
    { ...refusal("asksForSecret"), agent: agents.household, atText: "09:40:02", reason: "catalog forms 1.0 · component c2 TextField · obscured is refused (secret field)", component: "c2" },
  ];
}

/** What a home built with `genui` claims (as `adk_surface::genui::capabilities` answers). */
export function capabilities(): GenuiCapabilities {
  const claim = (id: string, label: string, status: GenuiCapabilities["protocols"][number]["claims"][number]["status"], scope: GenuiCapabilities["protocols"][number]["claims"][number]["scope"]) => ({ id, label, status, scope });
  return {
    protocols: [
      {
        id: "a2ui",
        name: "A2UI",
        claims: [
          claim("version_0_9_1", "Version 0.9.1", "supported", "runtime"),
          claim("version_1_0", "Version 1.0", "candidate", "runtime"),
          claim("trusted_catalog", "Trusted component catalog", "supported", "security"),
          claim("no_approval_controls", "No approval controls in drawn UI", "supported", "security"),
        ],
      },
      {
        id: "ag_ui",
        name: "AG-UI",
        claims: [
          claim("a2ui_surface_activity", "a2ui-surface activity", "planned", "transport"),
          claim("drawn_submit_interrupts", "Interrupts for a drawn submit", "planned", "transport"),
        ],
      },
      {
        id: "mcp_apps",
        name: "MCP Apps",
        claims: [
          claim("version_2026_01_26", "Version 2026-01-26", "planned", "host"),
          claim("sandbox_and_csp", "Sandbox and CSP", "planned", "host"),
          claim("app_tool_calls_governed", "App tool calls governed", "planned", "security"),
          claim("fullscreen", "Fullscreen and picture-in-picture", "planned", "client"),
        ],
      },
    ],
    catalogs: [
      {
        id: "https://adk-rust.com/a2ui/catalogs/forms/1.0/catalog.json",
        label: "forms 1.0",
        components: ["Button", "Card", "CheckBox", "ChoicePicker", "Column", "DateTimeInput", "Divider", "List", "Row", "Slider", "Tabs", "Text", "TextField"],
        leftOut: ["Image", "Icon", "Modal", "Video", "AudioPlayer"],
      },
    ],
  };
}

function Frame({ index, title, children }: { index: number; title: string; children: ReactNode }) {
  return (
    <figure className="genui-frame" style={{ margin: 0, display: "grid", gap: 10, alignContent: "start" }}>
      <figcaption style={{ fontSize: 13, fontWeight: 600 }}>
        <span className="mono" style={{ marginRight: 8 }}>
          {index}
        </span>
        {title}
      </figcaption>
      {children}
    </figure>
  );
}

/** A strip of frames that scrolls inside the window, as the app shell does not. */
const STRIP: CSSProperties = {
  position: "fixed",
  inset: 0,
  overflow: "auto",
  display: "grid",
  gridTemplateColumns: "repeat(auto-fill, minmax(360px, 1fr))",
  alignContent: "start",
  gap: 24,
  padding: 24,
  background: "var(--bg)",
};

const noSend = async () => undefined;
const sendLater = () => new Promise<void>(() => undefined);
const stale = async () => {
  throw new ApiError(409, "Couldn't send: this form changed — check it and send again", "surface.drawn_changed");
};

/** Every state of the drawn form, as the concept's strip shows them. */
function FormStates() {
  const sent = { actionId: "send", args: { to: "ana@example.com", amount: 4200, invoice: "INV-10" } };
  const send = async () => ({ type: "answered" }) as unknown as CommandOutcome;
  const frames: Array<[string, ReactNode]> = [
    ["Fresh", <DrawnView key="1" surface={invoiceForm()} onSend={noSend} />],
    ["Edited, not sent", <EditedForm key="2" />],
    ["Field error", <ErrorForm key="3" />],
    ["Sending", <SendingForm key="4" />],
    [
      "Sent and waiting: the ordinary card below",
      <DrawnView
        key="5"
        surface={invoiceForm()}
        state={sentState("waiting", "Sent at 10:03 · waiting for you")}
        sent={sent}
        below={<CardView card={formCard()} send={send} onSettled={() => undefined} />}
      />,
    ],
    ["Sent, done", <DrawnView key="6" surface={invoiceForm()} state={sentState("done", "Sent · done at 10:04")} sent={sent} />],
    ["Sent, you said no", <DrawnView key="7" surface={invoiceForm()} state={sentState("saidNo", "Sent · you said no")} sent={sent} />],
    ["Sent, failed", <DrawnView key="8" surface={invoiceForm()} state={sentState("failed", "Sent · failed")} sent={sent} />],
    [
      "Finance changed the form",
      <div key="9" style={{ display: "grid", gap: 12 }}>
        <DrawnView surface={invoiceForm(1)} superseded />
        <DrawnView surface={invoiceForm(2, 10, 5)} changed onSend={noSend} />
      </div>,
    ],
    [
      "Refused by policy",
      <DrawnView key="10" surface={invoiceForm()} state={sentState("refused", "Finance can't use send_invoice here", false)} onSend={noSend} />,
    ],
    ["Ended without an answer", <DrawnView key="11" surface={invoiceForm()} state={sentState("ended", "Ended without an answer")} sent={sent} />],
    ["Stale version", <StaleForm key="12" />],
  ];
  return (
    <main className="genui-states" style={STRIP}>
      {frames.map(([title, view], index) => (
        <Frame key={title} index={index + 1} title={title}>
          {view}
        </Frame>
      ))}
    </main>
  );
}

/** A frame that types into the form, then shows it. */
function Typed({ type, onSend, press }: { type: (form: HTMLElement) => void; onSend: () => Promise<void>; press?: boolean }) {
  return (
    <div
      ref={(element) => {
        if (!element || element.dataset.typed) return;
        element.dataset.typed = "yes";
        setTimeout(() => {
          type(element);
          if (press) element.querySelector<HTMLButtonElement>(".drawn-send")?.click();
        }, 0);
      }}
    >
      <DrawnView surface={invoiceForm()} onSend={onSend} />
    </div>
  );
}

/** Sets `input`'s value as a person typing would, so React sees it. */
function typeInto(input: HTMLInputElement | null, value: string | boolean) {
  if (!input) return;
  if (typeof value === "boolean") {
    if (input.checked !== value) input.click();
    return;
  }
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  setter?.call(input, value);
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

function EditedForm() {
  return <Typed onSend={noSend} type={(form) => typeInto(form.querySelector('input[type="checkbox"]'), true)} />;
}

function ErrorForm() {
  return (
    <Typed
      onSend={noSend}
      press
      type={(form) => {
        typeInto(form.querySelector('input[type="checkbox"]'), true);
        typeInto(form.querySelector("input.drawn-input"), "ana@");
      }}
    />
  );
}

function SendingForm() {
  return <Typed onSend={sendLater} press type={(form) => typeInto(form.querySelector('input[type="checkbox"]'), true)} />;
}

function StaleForm() {
  return <Typed onSend={stale} press type={(form) => typeInto(form.querySelector('input[type="checkbox"]'), true)} />;
}

/** The card and the table: shown, the long table cut, and changed. */
function DisplayStates() {
  return (
    <main style={STRIP}>
      <Frame index={1} title="Shown">
        <DrawnView surface={timesheetCard()} />
      </Frame>
      <Frame index={2} title="Long table cut at 10 rows">
        <DrawnView surface={hoursTable()} />
      </Frame>
      <Frame index={3} title="Finance changed it">
        <div style={{ display: "grid", gap: 12 }}>
          <DrawnView surface={timesheetCard(1)} superseded />
          <DrawnView surface={timesheetCard(2, 10, 5)} changed />
        </div>
      </Frame>
    </main>
  );
}

/** The refused notice's three wordings. */
function RefusedStates() {
  return (
    <main style={STRIP}>
      <Frame index={1} title="Looks like a decision">
        <RefusedNotice refusal={refusal("looksLikeDecision")} />
      </Frame>
      <Frame index={2} title="Something this home can't show">
        <RefusedNotice refusal={refusal("cannotShow")} />
      </Frame>
      <Frame index={3} title="A password field">
        <RefusedNotice refusal={refusal("asksForSecret")} />
      </Frame>
    </main>
  );
}

/** The decision of a Send: waiting, decided elsewhere, high risk, refused when it resumed. */
function CardStates() {
  const send = async () => ({ type: "answered" }) as unknown as CommandOutcome;
  const decided = formCard({
    version: 2,
    status: "decided",
    statusText: "Decided",
    allowed: { options: [], openInHome: false },
    timeline: [
      { text: "You pressed Send on a form Finance drew · 10:03", current: false },
      { text: "You said yes on your phone at 10:04", current: true },
    ],
    decided: { by: "You", byYou: true, at: at(10, 4), atText: "10:04", answer: "yes", text: "You said yes on your phone at 10:04" },
  });
  const high = formCard({
    risk: "high",
    riskText: "High risk",
    allowed: {
      options: [
        { answer: "allowOnce", label: "Confirm it's you and approve", needsPresence: true, needsExpanded: false },
        { answer: "edit", label: "Edit", needsPresence: false, needsExpanded: false },
        { answer: "deny", label: "Deny", needsPresence: false, needsExpanded: false },
      ],
      openInHome: false,
    },
  });
  return (
    <main style={{ ...STRIP, gridTemplateColumns: "repeat(auto-fill, minmax(420px, 1fr))" }}>
      <Frame index={1} title="Waiting">
        <CardView card={formCard()} send={send} onSettled={() => undefined} />
      </Frame>
      <Frame index={2} title="Decided elsewhere">
        <CardView card={decided} send={send} onSettled={() => undefined} />
      </Frame>
      <Frame index={3} title="High risk">
        <CardView card={high} send={send} onSettled={() => undefined} />
      </Frame>
      <Frame index={4} title="Refused when it resumed: a rule changed">
        <DrawnView surface={invoiceForm()} state={sentState("notRun", "Not run: Finance can't use send_invoice any more")} />
      </Frame>
    </main>
  );
}

/**
 * The fixture page of `?screen=genui&genui=`: `form`, `sent`, `display` and `refused` show the
 * home's Threads over Finance's thread drawn that way; `states`, `display-states`,
 * `refused-states` and `card-states` the concept strips; `protocols` the developer panel.
 */
export function genuiFixture(params: URLSearchParams): ReactNode {
  const variant = params.get("genui") ?? "form";
  switch (variant) {
    case "states":
      return <FormStates />;
    case "display-states":
      return <DisplayStates />;
    case "refused-states":
      return <RefusedStates />;
    case "card-states":
      return <CardStates />;
    case "protocols":
      return <Developer client={new GenuiFixtureClient("refused")} sessionId="s-finance" />;
    default: {
      const client = new GenuiFixtureClient(variant);
      return <App client={client} seams={fixtureSeams()} initialSection="threads" />;
    }
  }
}
