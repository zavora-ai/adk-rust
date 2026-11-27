// What the home says is what happened: a turn that failed says so where its reply would have
// been, the agent's notes between tool calls stay with its work, and a part of the home the
// server does not serve is never asked for (spec 3.3 §5.1, §6.1).
import { render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import type { CameraClient } from "../src/camera/client";
import { HttpClient, partOf } from "../src/client";
import { lines, Thread, worklog } from "../src/components/Thread";
import { applyFrame, emptyThread } from "../src/feed";
import { FixtureClient } from "../src/fixture/fixture";
import type { FeedEntry, Served, ThreadItem } from "../src/generated/api";
import type { VerbsClient } from "../src/verbs/client";
import type { WebsiteClient } from "../src/website/client";
import type { WorkflowClient } from "../src/workflow/client";

afterEach(() => vi.unstubAllGlobals());

const you = (text: string): ThreadItem => ({ kind: "message", from: "you", author: "You", text });
const agent = (text: string): ThreadItem => ({ kind: "message", from: "agent", author: "Claims Desk", text });
const call = (id: string, text: string): ThreadItem => ({ kind: "activity", tool: "vision_describe", text, callId: id });
const done = (id: string): ThreadItem => ({ kind: "result", tool: "vision_describe", ok: true, text: "Done", callId: id });
const failed: ThreadItem = {
  kind: "turnFailed",
  text: "Claims Desk couldn't answer",
  reason: "no recorded answer for turn 3",
  code: "model.internal",
};

function entries(...items: ThreadItem[][]): FeedEntry[] {
  return items.map((one, index) => ({ seq: index + 1, at: "2026-11-27T09:00:00Z", atText: "09:00", items: one }));
}

function thread(shown: FeedEntry[]) {
  return render(<Thread entries={shown} open={[]} client={new FixtureClient()} send={vi.fn()} onSettled={vi.fn()} />);
}

describe("a turn that failed", () => {
  it("says who could not answer and why, where the reply would have been", () => {
    const { container } = thread(entries([you("Is CL-0413 a total loss?"), failed]));
    const note = screen.getByRole("note");
    expect(note.textContent).toBe("Claims Desk couldn't answer: no recorded answer for turn 3model.internal");
    expect(container.querySelector(".message-agent")).toBeNull();
    // It follows the person's message.
    const order = [...container.querySelectorAll(".message-you, .turn-failed")].map((node) => node.className);
    expect(order).toEqual(["message message-you", "turn-failed"]);
  });

  it("reaches a thread already shown: the changed entry takes the place of the one with its seq", () => {
    const before = entries([you("Is CL-0413 a total loss?")]);
    let state = applyFrame(emptyThread("s1"), { type: "entry", entry: before[0]! });
    const changed = { ...before[0]!, items: [...before[0]!.items, failed] };
    state = applyFrame(state, { type: "changed", entry: changed });
    expect(state.entries).toEqual([changed]);
    expect(state.last).toBe(1);
    // An entry the thread never received changes nothing.
    expect(applyFrame(state, { type: "changed", entry: { ...changed, seq: 9 } })).toBe(state);
  });
});

describe("notes between tool calls", () => {
  const turn = entries(
    [you("Assess CL-0413")],
    [agent("Describe returned no damage boxes; retry with a more specific question."), call("c1", "Describe photo 1")],
    [done("c1")],
    [agent("The front bumper is dented; the estimate is $1,240.")],
  );

  it("are steps of the turn's work, and only the turn's last text is the reply", () => {
    const shown = lines(turn);
    const messages = shown.flatMap((line) => (line.kind === "message" ? [[line.from, line.interim === true]] : []));
    expect(messages).toEqual([
      ["you", false],
      ["agent", true],
      ["agent", false],
    ]);
  });

  it("read as a quiet note, never as a reply", () => {
    const { container } = thread(turn);
    const replies = [...container.querySelectorAll(".message-agent")].map((node) => node.textContent);
    expect(replies).toEqual(["The front bumper is dented; the estimate is $1,240."]);
    expect(container.querySelector(".work-note")?.textContent).toBe(
      "Describe returned no damage boxes; retry with a more specific question.",
    );
  });

  it("fold with the steps around them, which alone count as steps", () => {
    const busy = entries(
      [you("Assess CL-0413")],
      [agent("Looking at the photos."), call("c1", "Describe photo 1")],
      [done("c1"), agent("No boxes; asking again."), call("c2", "Describe photo 1 again")],
      [done("c2"), call("c3", "Describe photo 2")],
      [done("c3")],
      [agent("The front bumper is dented.")],
    );
    const units = worklog(lines(busy));
    expect(units.map((unit) => unit.kind)).toEqual(["line", "log", "line"]);
    const { container } = thread(busy);
    const log = screen.getByText("3 steps").closest("details")!;
    expect(within(log).getByText("No boxes; asking again.").closest(".work-note")).toBeTruthy();
    expect([...container.querySelectorAll(".message-agent")].map((node) => node.textContent)).toEqual([
      "The front bumper is dented.",
    ]);
  });

  it("leave a turn that ends on its text alone", () => {
    const shown = lines(entries([you("Hi")], [agent("Hello.")], [you("Bye")], [agent("Bye."), call("c1", "Close")], [done("c1")]));
    expect(shown.filter((line) => line.kind === "message" && line.interim).length).toBe(1);
    const hello = shown.find((line) => line.kind === "message" && line.text === "Hello.");
    expect(hello?.kind === "message" && hello.interim).toBeFalsy();
  });
});

describe("parts of the home the server does not serve", () => {
  it("names the part each optional route belongs to", () => {
    expect(partOf("/kernel/processes?state=waiting")).toBe("kernel");
    expect(partOf("/surface/cameras?surface=web")).toBe("cameras");
    expect(partOf("/surface/voice")).toBe("voice");
    expect(partOf("/surface/phone/devices")).toBe("phone");
    expect(partOf("/surface/status?surface=web")).toBeUndefined();
    expect(partOf("/surface/agents/finance/threads")).toBeUndefined();
    expect(partOf("/rules")).toBeUndefined();
  });

  function serving(serves: Served[] | undefined) {
    const seen: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string) => {
        seen.push(url);
        const body = url.includes("/surface/status")
          ? { decisions: 0, stopped: false, ...(serves ? { serves } : {}) }
          : url.includes("/kernel/processes")
            ? { processes: [] }
            : { models: [] };
        return new Response(JSON.stringify(body), { status: 200, headers: { "content-type": "application/json" } });
      }),
    );
    return seen;
  }

  it("asks for none of a part the status line leaves out", async () => {
    const seen = serving([]);
    const client = new HttpClient();
    expect(await client.processes()).toBeUndefined();
    expect(await client.agenda()).toBeUndefined();
    expect(await client.voiceModels()).toBeUndefined();
    expect(await client.whileAway().read()).toBeUndefined();
    expect(await client.limits().areas()).toBeUndefined();
    expect(seen).toEqual(["/api/surface/status?surface=web"]);
  });

  it("asks for a part the status line names, and every part of a server that does not say", async () => {
    let seen = serving(["kernel"]);
    expect(await new HttpClient().processes()).toEqual([]);
    expect(seen).toEqual(["/api/surface/status?surface=web", "/api/kernel/processes?state=waiting"]);
    seen = serving(undefined);
    await new HttpClient().voiceModels();
    expect(seen).toEqual(["/api/surface/status?surface=web", "/api/surface/voice"]);
  });

  it("never reads the website, screen actions, workflow runs or cameras of a server that serves none", async () => {
    const cameras = vi.fn(async () => undefined);
    class Bare extends FixtureClient {
      serves() {
        return Promise.resolve(new Set<Served>());
      }
      cameraClient() {
        return { cameras } as unknown as CameraClient;
      }
    }
    const website = { home: vi.fn(async () => undefined) } as unknown as WebsiteClient;
    const verbs = { list: vi.fn(async () => undefined), snooze: vi.fn() } as unknown as VerbsClient;
    const workflow = { graphs: vi.fn(async () => undefined) } as unknown as WorkflowClient;
    const client = new Bare();
    render(<App client={client} initialSection="threads" website={website} verbs={verbs} workflow={workflow} />);
    await screen.findByRole("navigation", { name: "Agents" });
    await waitFor(() => expect(screen.getByRole("button", { name: "Stop all" })).toBeTruthy());
    expect(website.home).not.toHaveBeenCalled();
    expect(verbs.list).not.toHaveBeenCalled();
    expect(workflow.graphs).not.toHaveBeenCalled();
    expect(cameras).not.toHaveBeenCalled();
  });
});
