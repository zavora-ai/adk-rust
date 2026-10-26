// Spec 3.3 §5.6 end to end, on `scripted_voice_home`: the microphone streams to the server, a
// spoken "allow once" runs the agent's call once after its cancel window, a high-risk call is
// not approvable by voice, and switching the screen off ends its track and its frames.
//
// The person's words and the model's turns come from the example's puppet routes; Chromium's
// fake microphone plays a tone, and a canvas stands in for the shared screen.
import type { APIRequestContext, Page } from "@playwright/test";
import { expect, test } from "@playwright/test";

const SEND = "printf x >> sent.txt";
const PUBLISH = "printf x >> published.txt";

interface Counts {
  audio: number;
  frames: number;
  sent: number;
  published: number;
}

async function counts(request: APIRequestContext): Promise<Counts> {
  return (await request.get("/puppet/counts")).json() as Promise<Counts>;
}

async function puppet(request: APIRequestContext, route: string, body: object) {
  const response = await request.post(`/puppet/${route}`, { data: body });
  expect(response.ok()).toBe(true);
}

/** The voice view, listening, its microphone streaming. */
async function listening(page: Page, request: APIRequestContext) {
  const before = (await counts(request)).audio;
  await page.goto("/ui/voice");
  await expect(page.getByRole("heading", { name: "Listening" })).toBeVisible({ timeout: 20_000 });
  await expect(page.getByRole("button", { name: "Mic on" })).toBeVisible();
  await expect.poll(async () => (await counts(request)).audio, { timeout: 10_000 }).toBeGreaterThan(before);
}

test("a spoken allow once runs the call once, after its cancel window", async ({ page, request }) => {
  await listening(page, request);
  const sent = (await counts(request)).sent;
  await puppet(request, "call", { command: SEND });
  await expect(
    page.getByText('Chief of staff wants to send the October invoice to [CLIENT]. Say "allow once" or "deny".'),
  ).toBeVisible({ timeout: 20_000 });
  // The prompt is on screen for a second before speech counts.
  await page.waitForTimeout(1_200);
  await puppet(request, "say", { text: "allow once" });
  const pending = page.getByRole("article", { name: "Send the October invoice to [CLIENT]" });
  await expect(pending.getByText('Heard "allow once"')).toBeVisible();
  await expect(pending.getByText(/^Allowing once in \d seconds?$/)).toBeVisible();
  expect((await counts(request)).sent).toBe(sent);
  await expect.poll(async () => (await counts(request)).sent, { timeout: 15_000 }).toBe(sent + 1);
  await page.waitForTimeout(1_000);
  expect((await counts(request)).sent).toBe(sent + 1);
});

test("a high-risk call is decided on screen, never approved by voice", async ({ page, request }) => {
  await listening(page, request);
  await puppet(request, "call", { command: PUBLISH });
  const line = page.getByText(`"Publish the release" is high risk, so voice can't approve it.`, { exact: false });
  await expect(line.first()).toBeVisible({ timeout: 20_000 });
  await expect(page.getByRole("button", { name: /^Allow/ })).toHaveCount(0);
  await page.waitForTimeout(1_200);
  await puppet(request, "say", { text: "allow once" });
  await expect(page.getByText("allow once").first()).toBeVisible();
  await page.waitForTimeout(1_500);
  await expect(page.getByText(/^Allowing once in/)).toHaveCount(0);
  expect((await counts(request)).published).toBe(0);
});

test("switching the screen off ends its track and its frames", async ({ page, request }) => {
  // The screen the person would pick: a canvas, drawn on every frame.
  await page.addInitScript(() => {
    navigator.mediaDevices.getDisplayMedia = async () => {
      const canvas = document.createElement("canvas");
      canvas.width = 640;
      canvas.height = 360;
      const draw = canvas.getContext("2d")!;
      let tick = 0;
      window.setInterval(() => {
        draw.fillStyle = tick++ % 2 ? "#f6f7f9" : "#e6e8ec";
        draw.fillRect(0, 0, 640, 360);
        draw.fillStyle = "#475569";
        draw.fillText(`[Shared window: invoice.pdf] ${tick}`, 200, 180);
      }, 200);
      const stream = canvas.captureStream(5);
      (window as unknown as { screenTrack: MediaStreamTrack }).screenTrack = stream.getVideoTracks()[0];
      return stream;
    };
  });
  await listening(page, request);
  const before = (await counts(request)).frames;
  await page.getByRole("button", { name: "Screen off" }).click();
  await expect(page.getByRole("button", { name: "Screen on" })).toBeVisible();
  await expect.poll(async () => (await counts(request)).frames, { timeout: 15_000 }).toBeGreaterThan(before);
  await page.getByRole("button", { name: "Screen on" }).click();
  await expect(page.getByRole("button", { name: "Screen off" })).toBeVisible();
  const state = await page.evaluate(() => (window as unknown as { screenTrack: MediaStreamTrack }).screenTrack.readyState);
  expect(state).toBe("ended");
  const after = (await counts(request)).frames;
  await page.waitForTimeout(2_500);
  expect((await counts(request)).frames).toBe(after);
});
