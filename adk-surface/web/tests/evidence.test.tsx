// Approve with the image (spec 3.7 §8.2, X11): the card shows the exact image that would
// leave, read as the viewer, with its regions and what was covered; Allow once names the digest
// shown; nothing to send leaves Allow once off; the editor previews a change and holds the call
// again with it; the phone draws the same card.
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { DecisionCard } from "../src/components/DecisionCard";
import { DecisionList } from "../src/components/DecisionList";
import { ImageLoaderContext } from "../src/evidence/images";
import { PhoneEvidence } from "../src/evidence/PhoneEvidence";
import type { Command, CommandOutcome, DecisionCard as Card, Origin } from "../src/generated/api";
import { visionCard } from "../src/fixture/vision";

const loaded: string[] = [];
function loader(href: string): Promise<Blob> {
  loaded.push(href);
  return Promise.resolve(new Blob([new Uint8Array([137, 80, 78, 71])], { type: "image/png" }));
}

function withImages(node: React.ReactNode) {
  return <ImageLoaderContext.Provider value={loader}>{node}</ImageLoaderContext.Provider>;
}

describe("approve with the image", () => {
  it("shows the exact image as the viewer, its regions, what was covered and where it goes", async () => {
    globalThis.URL.createObjectURL = vi.fn(() => "blob:card");
    globalThis.URL.revokeObjectURL = vi.fn();
    const card = visionCard("pending");
    render(withImages(<DecisionCard card={card} send={vi.fn()} />));
    const panel = screen.getByRole("region", { name: "The exact image that would leave" });
    await waitFor(() => expect(within(panel).getByRole("img")).toBeTruthy());
    expect(loaded).toContain(card.media!.images[0].href);
    expect(within(panel).getAllByText("personal text").length).toBe(6);
    expect(within(panel).getByText("face · whole person")).toBeTruthy();
    expect(within(panel).getByText("1 face")).toBeTruthy();
    expect(within(panel).getByText("api.anthropic.com")).toBeTruthy();
    expect(within(panel).getByText(card.media!.images[0].digestShort)).toBeTruthy();
    // "Image only" hides the regions; "Regions" shows them again (board choice A2).
    await userEvent.click(within(panel).getByRole("button", { name: "Image only" }));
    expect(within(panel).queryByText("face · whole person")).toBeNull();
    await userEvent.click(within(panel).getByRole("button", { name: "Regions" }));
    expect(within(panel).getByText("face · whole person")).toBeTruthy();
    expect(screen.getByText(card.media!.fixedLine)).toBeTruthy();
  });

  it("allows once naming the digest it showed (V-I5)", async () => {
    const card = visionCard("pending");
    const send = vi.fn(
      async (command: Command, origin: Origin): Promise<CommandOutcome> => (
        void origin, void command, { type: "answered", card: visionCard("decided") }
      ),
    );
    render(withImages(<DecisionCard card={card} send={send} />));
    await userEvent.click(screen.getByRole("button", { name: "Allow once" }));
    const [command] = send.mock.calls[0];
    expect(command).toMatchObject({ type: "decide", answer: "allowOnce", evidence: [card.media!.images[0].digest] });
  });

  it("leaves Allow once off when nothing can be sent, and Deny on (A13, A15)", () => {
    for (const state of ["purged", "allRedacted"] as const) {
      const { unmount } = render(withImages(<DecisionCard card={visionCard(state)} send={vi.fn()} />));
      const allow = screen.getByRole("button", { name: "Allow once" });
      expect((allow as HTMLButtonElement).disabled).toBe(true);
      expect((screen.getByRole("button", { name: "Deny" }) as HTMLButtonElement).disabled).toBe(false);
      expect(screen.getByRole("note").textContent).toMatch(state === "purged" ? /Nothing to send/ : /Everything is covered/);
      // A deleted image has no tools; a covered one shows them off: covering is never undone.
      const crop = screen.queryByRole("button", { name: "Tighten the crop" }) as HTMLButtonElement | null;
      expect(state === "purged" ? crop === null : crop?.disabled === true).toBe(true);
      unmount();
    }
  });

  it("previews a change, then holds the call again with it (A8)", async () => {
    const card = visionCard("pending");
    const commands: Command[] = [];
    const send = vi.fn(async (command: Command): Promise<CommandOutcome> => {
      commands.push(command);
      if (command.type === "changeImage" && command.preview) {
        return { type: "imagePreview", changed: { from: command.from, to: `sha256:${"7".repeat(64)}`, width: 600, height: 266 } };
      }
      const closed = visionCard("decided");
      const said = "You changed it at 10:15 · held again with the new image";
      return {
        type: "heldAgain",
        changed: { from: card.media!.images[0].digest, to: `sha256:${"7".repeat(64)}`, width: 600, height: 266 },
        card: { ...closed, decided: { ...closed.decided!, answer: "edited", text: said } },
      };
    });
    render(withImages(<DecisionCard card={card} send={send} />));
    await userEvent.click(screen.getByRole("button", { name: "Tighten the crop" }));
    const editor = screen.getByRole("dialog", { name: "Change the image before it leaves" });
    // Nothing changed yet: nothing to use.
    expect((within(editor).getByRole("button", { name: "Use this image" }) as HTMLButtonElement).disabled).toBe(true);
    // The bottom edge moves up by keyboard: the crop only shrinks.
    const bottom = within(editor).getByRole("button", { name: "Move the bottom edge" });
    bottom.focus();
    await userEvent.keyboard("{ArrowUp}{ArrowUp}{ArrowUp}");
    await waitFor(() => expect(within(editor).getByText(`sha256:777777…777777`)).toBeTruthy());
    const preview = commands.find((command) => command.type === "changeImage");
    expect(preview).toMatchObject({ type: "changeImage", preview: true, from: card.media!.images[0].digest });
    expect(preview && preview.type === "changeImage" && preview.change.crop![3]).toBeLessThan(380);
    await userEvent.click(within(editor).getByRole("button", { name: "Use this image" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(commands.at(-1)).toMatchObject({ type: "changeImage", preview: false });
    expect(screen.getByText(/Held again with sha256:777777…777777/)).toBeTruthy();
  });

  it("shows each decision's image, small, in Decisions (A10)", () => {
    render(withImages(<DecisionList decisions={[visionCard("pending")]} onOpen={vi.fn()} />));
    expect(document.querySelector(".evidence-thumb")).not.toBeNull();
  });

  it("draws the phone's card: the image, what was covered, where it goes, the cost", () => {
    const card: Card = visionCard("pending");
    render(withImages(<PhoneEvidence card={card} media={card.media!} />));
    expect(screen.getByText("The exact image that would leave")).toBeTruthy();
    expect(screen.getAllByText("text").length).toBe(6);
    expect(screen.getByText("6 personal text")).toBeTruthy();
    expect(screen.getByText("Up to $0.0070")).toBeTruthy();
  });
});
