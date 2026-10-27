// Generated from adk-surface's workflow view types by `scripts/check-surface-api.sh --write`.
// Do not edit: CI fails when this file drifts from the Rust source (spec 3.3 §12).

/** How a state key changed in a step. */
export type Change = "added" | "changed" | "removed";

/** One channel's value at a checkpoint, for the fork dialog's editor. */
export interface ChannelValue {
  /** The channel. */
  key: string;
  /** Why the value is not shown as it is. */
  note?: string | null;
  /** Its value, shown as every value is; `null` when the state does not hold it. */
  value: unknown;
}

/** One edge of a `GraphView`. */
export interface EdgeView {
  /** Whether a router chooses the edge at run time. */
  conditional: boolean;
  /** The node the edge leaves. */
  from: string;
  /** The node the edge reaches, or `__end__`. */
  to: string;
}

/** A governed call that already happened, from the effect log (spec 3.3 §12). */
export interface EffectView {
  /** When it happened. */
  at: string;
  /** `09:14`. */
  atText: string;
  /** The call in short: `POST 127.0.0.1/charges`. */
  call?: string | null;
  /** The governed node. */
  node: string;
  /** "pay — POST 127.0.0.1/charges at 09:14". */
  text: string;
  /** The keys it wrote. */
  writes: string[];
}

/** What a fork did (spec 3.3 §12). */
export interface ForkOutcome {
  /** The keys it overwrote. */
  edited: string[];
  /** The checkpoint it was forked from. */
  from: string;
  /** The fork's new session. */
  sessionId: string;
  /** Whether its run started. */
  started: boolean;
  /** "Forked. The original run is unchanged." */
  text: string;
  /** Its checkpoint thread. */
  threadId: string;
}

/** What a fork from a step would do, for the fork dialog (spec 3.3 §12). */
export interface ForkPoint {
  /** Whether a fork may start here. */
  allowed: boolean;
  /**
   * Governed calls after this step that already happened and are answered from their
   * record unless the fork changes them.
   */
  alreadyHappened: EffectView[];
  /** Why not: "Nothing runs after the last step". */
  refusal?: string | null;
  /** The nodes that run again: `pay`, `receipt`. */
  runsAgain: string[];
}

/** `POST /surface/workflow/sessions/{session}/fork` (spec 3.3 §12). */
export interface ForkRequest {
  /** The completed checkpoint of the step to fork from. */
  from: string;
  /** Channels to overwrite in the fork, whole. */
  set?: Record<string, unknown>;
  /** Start the fork's run now. */
  start?: boolean;
}

/** A fork of a run, as "Forks of this run" lists it. */
export interface ForkSummary {
  /** When the fork started, or was made. */
  at: string;
  /** `09:31`. */
  atText: string;
  /** The keys it overwrote. */
  edited: string[];
  /** The step it was forked at. */
  parentStep: number;
  /** The fork's session. */
  sessionId: string;
  /** "from step 1 · edited note". */
  text: string;
}

/** A graph agent the observer shows: its nodes, edges and channels (spec 3.3 §12). */
export interface GraphView {
  /** The state channels the graph declares, which a fork may edit. */
  channels: string[];
  /** The graph agent's description. */
  description: string;
  /** Every edge; `to` is `__end__` for an edge that ends a branch. */
  edges: EdgeView[];
  /** The nodes a run starts with. */
  entry: string[];
  /** The graph agent's name: `invoice-run`. */
  name: string;
  /** Every node, sorted by name. */
  nodes: NodeView[];
}

/** `GET /surface/workflow/graphs`. */
export interface GraphsResponse {
  /** Every graph agent the home observes. */
  graphs: GraphView[];
}

/** The decision a governed node waited on, from the inbox. */
export interface NodeDecision {
  /** The decision's id. */
  id: string;
  /** "Asked at 09:14", or who answered: "You said yes from the home at 09:15". */
  text: string;
  /** Whether it still waits. */
  waiting: boolean;
}

/** One node of a step detail. */
export interface NodeDetail {
  /** The decision a governed node waited on. */
  decision?: NodeDecision | null;
  /**
   * For a replayed call, the record that answered it: "Not sent again — the record from
   * 09:14 answered it".
   */
  effect?: EffectView | null;
  /** "Model step", "HTTP call", "Function". */
  kindText: string;
  /** A model step: its model and tool activity is in the session's thread. */
  modelStep: boolean;
  /** What became of the node. */
  run: NodeRun;
  /** A line under the node: "Not sent again — the record from 09:14 answered it". */
  text?: string | null;
}

/** One node's part in a run or a step (spec 3.3 §12). */
export interface NodeRun {
  /** Its attempt count when it finished. */
  attempt?: number | null;
  /** The decision a waiting node waits on, from the inbox. */
  decisionId?: string | null;
  /** How long it ran, in milliseconds. */
  durationMs?: number | null;
  /** That duration in words: `1.2 s`. */
  durationText?: string | null;
  /** The error it failed or was refused with. */
  error?: string | null;
  /** For a fork, the time the original run did this node: "from 09:14". */
  inheritedText?: string | null;
  /** The node's name. */
  node: string;
  /** When it started. */
  startedAt?: string | null;
  /** That time as the home shows it: `09:14:01`. */
  startedText?: string | null;
  /** What became of it. */
  state: NodeState;
  /** In words: "Done", "Waiting for you", "Not sent again", "Not run", … */
  stateText: string;
  /** The state keys it wrote, sorted. */
  writes?: string[];
}

/** What became of a node, as the run view shows it. */
export type NodeState = "idle" | "working" | "done" | "waiting" | "paused" | "failed" | "recorded" | "notRun";

/** One node of a `GraphView`. */
export interface NodeView {
  /** "Asks first", "Spends money", "Can't be undone". */
  badges: string[];
  /** The node's description. */
  description: string;
  /** What kind of step it is, as adk-graph names it: `modelStep`, `function`, `httpCall`, … */
  kind: string;
  /** The kind in words: "Model step", "HTTP call", "Function". */
  kindText: string;
  /** The node's name. */
  name: string;
}

/** Where a forked thread came from (spec 3.3 §12). */
export interface RunLineage {
  /** Whether the edits were checked against the graph's channels. */
  checked: boolean;
  /** The keys the fork overwrote. */
  edited: string[];
  /** The thread this one was forked from. */
  parent: string;
  /** The checkpoint of `parent` it was forked at. */
  parentCheckpoint: string;
  /** The session of `parent`, when it is the caller's. */
  parentSession?: string | null;
  /** That checkpoint's step label. */
  parentStep: number;
  /** The thread the family of forks started from. */
  root: string;
  /** "Fork of the 09:14 run at step 1 · edited note". */
  text: string;
}

/** How a run stands (spec 3.3 §12). */
export type RunStatus = "noRun" | "notStarted" | "working" | "waiting" | "paused" | "stopped" | "done" | "failed";

/**
 * One step of a run, as the Steps list shows it (spec 3.3 §12).
 * 
 * A step that paused and later completed is one row, addressed by its completed
 * checkpoint; "Step n" is a label, never an address.
 */
export interface RunStep {
  /** When it started. */
  at: string;
  /** That time as the home shows it: `09:14:01`. */
  atText: string;
  /**
   * The checkpoint the step detail and a fork read: the completed checkpoint of the step,
   * else the one its pause or failure wrote. Absent for the input.
   */
  checkpointId?: string | null;
  /** Whether a fork may start here: a completed step with work after it. */
  forkable: boolean;
  /** "Step 1". */
  label: string;
  /** The step's nodes. */
  nodes: NodeRun[];
  /** How the step ended. */
  state: StepState;
  /** The step's label. */
  step: number;
  /** "Your message" for the input, "nothing written yet" for a step that waits. */
  text?: string | null;
  /** The keys the step wrote, sorted. */
  wrote: string[];
}

/** One run of a session, as `GET …/runs` lists it, from its records only. */
export interface RunSummary {
  /** The run's id. */
  runId: string;
  /** When it started. */
  startedAt: string;
  /** `09:14`. */
  startedText: string;
  /** Its steps, oldest first. */
  steps: RunStep[];
}

/** `GET /surface/workflow/sessions/{session}/runs`. */
export interface RunsResponse {
  /** The graph agent. */
  graph: string;
  /** Where the thread was forked from. */
  lineage?: RunLineage | null;
  /** The first step still kept, when retention removed the ones before it. */
  removedBefore?: number | null;
  /** Every run whose records are kept, oldest first. */
  runs: RunSummary[];
  /** The session. */
  sessionId: string;
  /** The session's checkpoint thread. */
  threadId: string;
}

/**
 * One key of "What changed" (spec 3.3 §12): computed here so the web pane and the terminal
 * show the same diff. Values pass secret redaction, the host's hidden keys and a size cap.
 */
export interface StateChange {
  /** The value after; absent when removed. */
  after?: unknown;
  /** The value before; absent when added. */
  before?: unknown;
  /** How it changed. */
  change: Change;
  /** "Added", "Changed" or "Removed". */
  changeText: string;
  /** The key. */
  key: string;
  /** Why a value is not shown as it is: "Hidden: looks like a secret". */
  note?: string | null;
}

/**
 * `GET /surface/workflow/sessions/{session}/checkpoints/{id}`: one step, from one
 * checkpoint (spec 3.3 §12). Reading it changes nothing.
 */
export interface StepDetail {
  /** When it started: `09:14:03`. */
  atText: string;
  /** What changed, by key. */
  changes: StateChange[];
  /** The checkpoint. */
  checkpointId: string;
  /** What a fork from here would do. */
  fork: ForkPoint;
  /** "Step 2". */
  label: string;
  /** The step's nodes. */
  nodes: NodeDetail[];
  /** The session. */
  sessionId: string;
  /** How the step ended. */
  state: StepState;
  /** The state at this checkpoint, one row per channel. */
  stateValues: ChannelValue[];
  /** The step's label. */
  step: number;
}

/** How a step ended, as the run view lists it. */
export type StepState = "input" | "completed" | "paused" | "failed";

/** A `changed` frame of the feed: the thread's newest checkpoint version. */
export interface WorkflowChanged {
  /** The newest checkpoint version. */
  version: number;
}

/**
 * A run of a session's graph, as the Runs tab shows it (spec 3.3 §12).
 * 
 * Built from the checkpoints' run and step records, the effect log and the inbox; never
 * from agent output.
 */
export interface WorkflowRun {
  /** From the start to its latest checkpoint, in milliseconds. */
  durationMs?: number | null;
  /** The error a failed run ended with. */
  error?: string | null;
  /** The forks made from this thread, newest first. */
  forks?: ForkSummary[];
  /** The graph agent. */
  graph: string;
  /** Where the thread was forked from. */
  lineage?: RunLineage | null;
  /** Every node of the graph, as it stands in this run. */
  nodes: NodeRun[];
  /** The question a paused node asked: "Which month should the invoice cover?". */
  question?: string | null;
  /**
   * The first step still kept, when retention removed the ones before it: "Steps before 12
   * were removed".
   */
  removedBefore?: number | null;
  /** The run's id: the invocation that started it. Absent before the first run. */
  runId?: string | null;
  /** The session. */
  sessionId: string;
  /** When the run started. */
  startedAt?: string | null;
  /** How the run stands. */
  status: RunStatus;
  /** In words: "Waiting for you", "Done", … */
  statusText: string;
  /** The run's steps, oldest first. */
  steps: RunStep[];
  /** "Started 09:14 · 4.2 s · 3 steps", or what there is to say before a run. */
  summary: string;
  /** The edges the run took. */
  taken: EdgeView[];
  /** The session's checkpoint thread. */
  threadId: string;
}
