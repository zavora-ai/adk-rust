/**
 * The workflow observer's example (spec 3.3 §12, "Approve, rewind, fork"): `invoice-run`,
 * draft → pay → receipt, waiting at pay, done, or forked. `?screen=workflow&run=done` (or
 * `fork`, `noRun`, `failed`, `stopped`, `paused`) picks the run; the default waits at pay.
 */
import type { DecisionCard } from "../generated/api";
import type {
  ForkOutcome,
  ForkRequest,
  GraphView,
  NodeRun,
  RunStep,
  StepDetail,
  WorkflowChanged,
  WorkflowRun,
} from "../generated/workflow";
import { ApiError } from "../client";
import type { WorkflowClient } from "../workflow/client";
import type { FixtureClient } from "./fixture";
import { invoiceCard } from "./fixture";

export const SESSION = "s-7f21";
export const FORK_SESSION = "s-b3e8";
const THREAD = `adk-rust:ana:${SESSION}`;
export const DECISION = "apr_charge";

export function invoiceRun(): GraphView {
  return {
    name: "invoice-run",
    description: "Drafts an invoice, charges it once a person approves, files the receipt",
    nodes: [
      { name: "draft", description: "", kind: "modelStep", kindText: "Model step", badges: [] },
      {
        name: "pay",
        description: "",
        kind: "httpCall",
        kindText: "HTTP call",
        badges: ["Asks first", "Spends money", "Can't be undone"],
      },
      { name: "receipt", description: "", kind: "function", kindText: "Function", badges: [] },
    ],
    edges: [
      { from: "draft", to: "__end__", conditional: true },
      { from: "draft", to: "pay", conditional: true },
      { from: "pay", to: "receipt", conditional: false },
      { from: "receipt", to: "__end__", conditional: false },
    ],
    entry: ["draft"],
    channels: ["charge", "invoice", "note", "receipt"],
  };
}

function node(name: string, state: NodeRun["state"], stateText: string, extra: Partial<NodeRun> = {}): NodeRun {
  return { node: name, state, stateText, ...extra };
}

const draftDone = node("draft", "done", "Done", { startedText: "09:14:01", durationMs: 1200, durationText: "1.2 s", attempt: 1, writes: ["invoice"] });
const payWaiting = node("pay", "waiting", "Waiting for you", { startedText: "09:14:03", attempt: 1, decisionId: DECISION });
const payDone = node("pay", "done", "Done", { startedText: "09:15:02", durationMs: 300, durationText: "0.3 s", attempt: 1, writes: ["charge"] });
const receiptDone = node("receipt", "done", "Done", { startedText: "09:15:03", durationMs: 100, durationText: "0.1 s", attempt: 1, writes: ["receipt"] });

function step(n: number, at: string, state: RunStep["state"], nodes: NodeRun[], wrote: string[], extra: Partial<RunStep> = {}): RunStep {
  return {
    step: n,
    label: `Step ${n}`,
    state,
    at: `2026-10-27T06:${at}Z`,
    atText: `09:${at}`,
    checkpointId: state === "input" ? undefined : `cp-${n}`,
    nodes,
    wrote,
    forkable: false,
    ...extra,
  };
}

const input = step(0, "14:00", "input", [], ["note"], { text: "Your message" });
const drafted = step(1, "14:01", "completed", [draftDone], ["invoice"], { forkable: true });

export function waitingRun(): WorkflowRun {
  return {
    sessionId: SESSION,
    threadId: THREAD,
    graph: "invoice-run",
    runId: "inv-01JB7Q3K",
    status: "waiting",
    statusText: "Waiting for you",
    summary: "Started 09:14 · 4.2 s · 3 steps",
    nodes: [draftDone, payWaiting, node("receipt", "idle", "Idle")],
    taken: [{ from: "draft", to: "pay", conditional: true }],
    steps: [input, drafted, step(2, "14:03", "paused", [payWaiting], [], { text: "nothing written yet" })],
  };
}

export function doneRun(): WorkflowRun {
  return {
    ...waitingRun(),
    status: "done",
    statusText: "Done",
    summary: "Started 09:14 · 5.0 s · 4 steps",
    nodes: [draftDone, payDone, receiptDone],
    taken: [
      { from: "draft", to: "pay", conditional: true },
      { from: "pay", to: "receipt", conditional: false },
      { from: "receipt", to: "__end__", conditional: false },
    ],
    steps: [
      input,
      drafted,
      step(2, "14:03", "completed", [payDone], ["charge"], { forkable: true }),
      step(3, "15:03", "completed", [receiptDone], ["receipt"]),
    ],
    forks: [{ sessionId: FORK_SESSION, at: "2026-10-27T06:31:00Z", atText: "09:31", parentStep: 1, edited: ["note"], text: "from step 1 · edited note" }],
  };
}

export function forkRun(): WorkflowRun {
  const recorded = node("pay", "recorded", "Not sent again", { startedText: "09:31:00", attempt: 0, writes: ["charge"] });
  return {
    sessionId: FORK_SESSION,
    threadId: `adk-rust:ana:${FORK_SESSION}`,
    graph: "invoice-run",
    runId: "inv-01JB8A2D",
    status: "done",
    statusText: "Done",
    summary: "Started 09:31 · 0.4 s · 2 steps",
    nodes: [{ ...draftDone, inheritedText: "from 09:14" }, recorded, receiptDone],
    taken: doneRun().taken,
    steps: [
      step(2, "31:00", "completed", [recorded], ["charge"], { forkable: true }),
      step(3, "31:00", "completed", [receiptDone], ["receipt"]),
    ],
    lineage: {
      root: THREAD,
      parent: THREAD,
      parentSession: SESSION,
      parentCheckpoint: "cp-1",
      parentStep: 1,
      edited: ["note"],
      checked: true,
      text: "Fork of the 09:14 run at step 1 · edited note",
    },
  };
}

export function stepDetail(): StepDetail {
  return {
    sessionId: SESSION,
    checkpointId: "cp-1",
    step: 1,
    label: "Step 1",
    state: "completed",
    atText: "09:14:01",
    nodes: [{ run: draftDone, kindText: "Model step", modelStep: true }],
    changes: [
      {
        key: "invoice",
        change: "added",
        changeText: "Added",
        after: { customer: "Acme", amountCents: 4200 },
      },
    ],
    stateValues: [
      { key: "charge", value: null, editable: true },
      { key: "invoice", value: { customer: "Acme", amountCents: 4200 }, editable: true },
      { key: "note", value: "Bill Acme for October", editable: true },
      { key: "receipt", value: null, editable: true },
    ],
    fork: {
      allowed: true,
      runsAgain: ["pay", "receipt"],
      changedCalls: "A changed call waits for you.",
      alreadyHappened: [
        {
          node: "pay",
          call: "POST 127.0.0.1:8795/charges",
          at: "2026-10-27T06:14:00Z",
          atText: "09:14",
          writes: ["charge"],
          text: "pay — POST 127.0.0.1:8795/charges at 09:14",
        },
      ],
    },
  };
}

/** The pay node's card, as the inbox draws it. */
export function chargeCard(): DecisionCard {
  return {
    ...invoiceCard(),
    id: DECISION,
    sessionId: SESSION,
    agent: { name: "invoice-run", displayName: "invoice-run", avatar: { monogram: "I", color: "#1D4ED8" } },
    sinceText: "since 09:14",
    heading: "Charge Acme 42.00 USD",
    call: {
      tool: "pay",
      args: {
        method: "POST",
        url: "http://127.0.0.1:8795/charges",
        headers: { "content-type": "application/json" },
        body: { customer: "Acme", amountCents: "4200" },
      },
      text: 'pay(method: "POST", url: "http://127.0.0.1:8795/charges", …)',
      truncated: false,
    },
    timeline: [
      { text: "Asked at 09:14", current: false },
      { text: "Waiting for you", current: true },
    ],
    allowed: {
      options: [
        { answer: "allowOnce", label: "Confirm it's you and approve", needsPresence: true, needsExpanded: false },
        { answer: "edit", label: "Edit", needsPresence: true, needsExpanded: false },
        { answer: "deny", label: "Deny", needsPresence: false, needsExpanded: false },
      ],
      openInHome: false,
    },
    risk: "high",
    riskText: "High risk",
    reversibility: "cantBeUndone",
    reversibilityText: "Can't be undone",
    money: true,
    alsoShown: "Also in Decisions",
    detail: { text: "From the invoice-run workflow", linkText: "Open in Runs", href: `/ui/developer/runs/${SESSION}` },
  };
}

/** The observer's routes over the example, recording every fork asked for. */
export class WorkflowFixture implements WorkflowClient {
  readonly forks: ForkRequest[] = [];
  private runs = new Map<string, WorkflowRun>();

  constructor(
    initial: WorkflowRun = waitingRun(),
    private readonly absent = false,
  ) {
    this.runs.set(initial.sessionId, initial);
    if (initial.sessionId !== FORK_SESSION) this.runs.set(FORK_SESSION, forkRun());
  }

  async graphs() {
    return this.absent ? undefined : [invoiceRun()];
  }

  async run(session: string) {
    const run = this.runs.get(session);
    if (!run) throw new ApiError(404, `session '${session}' was not found`, "surface.not_found");
    return run;
  }

  async step() {
    return stepDetail();
  }

  async fork(_session: string, request: ForkRequest): Promise<ForkOutcome> {
    this.forks.push(request);
    return {
      sessionId: FORK_SESSION,
      threadId: `adk-rust:ana:${FORK_SESSION}`,
      from: request.from,
      edited: Object.keys(request.set ?? {}),
      started: request.start ?? false,
      text: "Forked. The original run is unchanged.",
    };
  }

  async start(): Promise<ForkOutcome> {
    throw new ApiError(409, "Forked. Everything is stopped — start it after Resume all.", "workflow.stopped");
  }

  // eslint-disable-next-line require-yield
  async *changes(): AsyncIterable<WorkflowChanged> {
    return;
  }
}

/** The fixture's workflow for `?screen=workflow`, its card held in `client`'s inbox. */
export function workflowFixture(params: URLSearchParams, client: FixtureClient): WorkflowFixture {
  client.hold(chargeCard());
  const runs: Record<string, () => WorkflowRun> = { waiting: waitingRun, done: doneRun, fork: forkRun };
  return new WorkflowFixture((runs[params.get("run") ?? "waiting"] ?? waitingRun)());
}
