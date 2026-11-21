// Images a person sends and the commands an agent runs (spec 3.3 §5.1, §5.7): the message box
// takes images picked, pasted or dropped and sends them with the text; it refuses what the
// engine would; the thread shows the person's images and each run with its last lines, exit
// and where it ran.
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { Composer } from "../src/components/Composer";
import { lines, Thread } from "../src/components/Thread";
import { CI_SCREENSHOT, codeThread, FixtureClient } from "../src/fixture/fixture";
import type { Command, ShellView } from "../src/generated/api";

const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13]);
const png = (name = "shot.png") => new File([PNG], name, { type: "image/png" });

function box(onSend = vi.fn().mockResolvedValue(undefined)) {
  render(<Composer attach placeholder="Message Code" queued={[]} onSend={onSend} />);
  return onSend;
}

describe("the message box takes images", () => {
  it("sends a picked image with the text, then clears both", async () => {
    const onSend = box();
    const picker = document.querySelector<HTMLInputElement>('input[type="file"]')!;
    expect(picker.accept).toBe("image/png,image/jpeg,image/webp,image/gif");
    await userEvent.upload(picker, png());
    const tray = await screen.findByRole("list", { name: "Images to send" });
    expect(within(tray).getByRole("button", { name: "Remove image 1" })).toBeTruthy();
    expect(within(tray).getByText("1 of 4 · paste, drop or pick images")).toBeTruthy();
    await userEvent.type(screen.getByLabelText("Message Code"), "Why does this fail?");
    await userEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(onSend).toHaveBeenCalledWith("Why does this fail?", { kind: "pointer" }, [
      { mimeType: "image/png", data: btoa(String.fromCharCode(...PNG)) },
    ]);
    await waitFor(() => expect(screen.queryByRole("list", { name: "Images to send" })).toBeNull());
    expect((screen.getByLabelText("Message Code") as HTMLInputElement).value).toBe("");
  });

  it("sends an image alone, and a pasted one", async () => {
    const onSend = box();
    fireEvent.paste(screen.getByLabelText("Message Code"), { clipboardData: { files: [png("pasted.png")] } });
    await screen.findByRole("button", { name: "Remove image 1" });
    await userEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(onSend).toHaveBeenCalledWith("", { kind: "pointer" }, [expect.objectContaining({ mimeType: "image/png" })]);
  });

  it("refuses what the engine would refuse, and keeps four at most", async () => {
    box();
    const picker = document.querySelector<HTMLInputElement>('input[type="file"]')!;
    await userEvent.upload(picker, new File(["<svg onload=alert(1)>"], "x.svg", { type: "image/svg+xml" }), { applyAccept: false });
    expect((await screen.findByRole("alert")).textContent).toBe("Send PNG, JPEG, WebP or GIF images.");
    await userEvent.upload(picker, [png("1.png"), png("2.png"), png("3.png"), png("4.png"), png("5.png")]);
    await waitFor(() => expect(screen.getAllByRole("button", { name: /^Remove image/ })).toHaveLength(4));
    expect(screen.getByRole("alert").textContent).toBe("A message carries at most 4 images.");
    expect((screen.getByRole("button", { name: "Attach images" }) as HTMLButtonElement).disabled).toBe(true);
    await userEvent.click(screen.getByRole("button", { name: "Remove image 2" }));
    expect(screen.getAllByRole("button", { name: /^Remove image/ })).toHaveLength(3);
  });

  it("offers nothing to attach where the box takes text only", () => {
    render(<Composer placeholder="Message Code" queued={[]} onSend={vi.fn()} />);
    expect(screen.queryByRole("button", { name: "Attach images" })).toBeNull();
  });
});

describe("the thread shows images and runs", () => {
  const seatbelt: ShellView = { place: { kind: "sandbox", enforcer: "seatbelt" }, text: "Commands run in the sandbox (Seatbelt)" };
  const thread = (shell?: ShellView) =>
    render(<Thread entries={codeThread()} open={[]} client={new FixtureClient()} send={vi.fn()} onSettled={vi.fn()} shell={shell} />);

  it("joins each run to its activity", () => {
    const shown = lines(codeThread());
    expect(shown.map((line) => line.kind)).toEqual(["message", "message", "activity", "activity"]);
    expect(shown.filter((line) => line.kind === "activity").map((line) => (line.kind === "activity" ? line.run?.tool : ""))).toEqual(["bash", "sandbox_exec"]);
  });

  it("shows the person's image, opened large on a press", async () => {
    thread(seatbelt);
    const open = screen.getByRole("button", { name: "Open image 1 of 1" });
    expect(open.querySelector("img")?.getAttribute("src")).toBe(`data:image/png;base64,${CI_SCREENSHOT}`);
    expect(screen.getByText("The build fails like this on CI. Can you find why?")).toBeTruthy();
  });

  it("shows a run's command, exit, time, place and last lines, open when it failed", () => {
    thread(seatbelt);
    const run = screen.getByText("cargo test -p parser", { exact: false }).closest("details")!;
    expect(run.open).toBe(true);
    const head = within(run.querySelector("summary")!);
    expect(head.getByText("Exit 101")).toBeTruthy();
    expect(head.getByText("14.2 s")).toBeTruthy();
    expect(head.getByText("Sandbox").getAttribute("title")).toBe("Commands run in the sandbox (Seatbelt)");
    const output = within(run).getByLabelText("Output");
    expect(output.querySelectorAll(".run-stderr")).toHaveLength(2);
    expect(within(run).getByText("Showing the last 8 lines of 214.")).toBeTruthy();
    expect(within(run).getByText("The agent can read all of it.")).toBeTruthy();
  });

  it("says the sandbox's own runs ran in the sandbox, and why one stopped", () => {
    thread(undefined);
    const run = screen.getByText("Ran python: import urllib.request", { exact: false }).closest("details")!;
    const head = within(run.querySelector("summary")!);
    expect(head.getByText("Refused by the sandbox")).toBeTruthy();
    expect(head.getByText("Sandbox")).toBeTruthy();
    expect(head.getByText("python")).toBeTruthy();
    // Without the agent's shell, a bash run claims no place.
    const bash = screen.getByText("cargo test -p parser", { exact: false }).closest("details")!;
    expect(within(bash.querySelector("summary")!).queryByText("Sandbox")).toBeNull();
  });

  it("calls a run on this computer what it is", () => {
    thread({ place: { kind: "host" }, text: "Commands run on this computer" });
    const bash = screen.getByText("cargo test -p parser", { exact: false }).closest("details")!;
    expect(within(bash.querySelector("summary")!).getByText("On this computer")).toBeTruthy();
  });
});

describe("the threads screen", () => {
  it("says where the agent's commands run, and sends images with the message", async () => {
    const client = new FixtureClient();
    const command = vi.spyOn(client, "command");
    render(<App client={client} initialSection="threads" />);
    const roster = await screen.findByRole("navigation", { name: "Agents" });
    await userEvent.click(within(roster).getByRole("button", { name: /Code/ }));
    expect(await screen.findByText("Commands run in the sandbox (Seatbelt)")).toBeTruthy();
    const picker = document.querySelector<HTMLInputElement>('input[type="file"]')!;
    await userEvent.upload(picker, png());
    await screen.findByRole("button", { name: "Remove image 1" });
    await userEvent.type(screen.getByLabelText("Message Code"), "and now?");
    await userEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(command).toHaveBeenCalled());
    const sent = command.mock.calls.at(-1)![0] as Extract<Command, { type: "send" }>;
    expect(sent.type).toBe("send");
    expect(sent.text).toBe("and now?");
    expect(sent.images).toEqual([{ mimeType: "image/png", data: btoa(String.fromCharCode(...PNG)) }]);
  });
});
