// Spec 3.7 §8.2 and S12 end to end, on the real frame pipeline (`live_camera_home`): the
// conveyor clip over RTSP, each camera through the stream pipeline. The overlays are the
// pipeline's and draw within one frame interval; line 3's small quota steps its governor down
// and the board says so from the governor's numbers; a zone edited here changes which cracks
// fire; switching off closes the camera's RTSP connection, so nothing is captured, and the
// server's gate drops anything still on its way; a card raised from the frame holds the
// Inspector's line stop with that frame as its evidence.
import { expect, test } from "@playwright/test";
import type { APIRequestContext, Page } from "@playwright/test";

// `CAMERA_E2E_DETECTOR=rf-detr` with `CAMERA_E2E_BIN` names a build of the same home with RF-DETR
// Nano, which finds the clip's wall clock rather than its parts: line 2 then watches for the clock.
const rfDetr = process.env.CAMERA_E2E_DETECTOR === "rf-detr";

interface Counts {
  pipeline: { arrived: number; dropped: number; observed: number; fired: number };
  hubDropped: number;
  on: boolean;
  capturing: boolean;
}

async function counts(request: APIRequestContext, camera: string): Promise<Counts> {
  const all = (await (await request.get("/e2e/cameras")).json()) as Record<string, Counts>;
  return all[camera];
}

async function view(request: APIRequestContext, camera: string) {
  return (await request.get(`/api/surface/cameras/${camera}?surface=web`)).json();
}

/** Draws a new zone over the picture from one corner to the other, in the camera's pixels. */
async function drawZone(page: Page, from: [number, number], to: [number, number], cameraWidth: number) {
  await page.getByRole("button", { name: "Draw a zone" }).click();
  const editor = page.getByRole("dialog", { name: "Draw or edit a zone" });
  await editor.getByRole("button", { name: "Draw a new zone" }).click();
  const layer = editor.locator(".cam-editor-layer");
  const box = (await layer.boundingBox())!;
  const scale = box.width / cameraWidth;
  await page.mouse.move(box.x + from[0] * scale, box.y + from[1] * scale);
  await page.mouse.down();
  await page.mouse.move(box.x + ((from[0] + to[0]) / 2) * scale, box.y + ((from[1] + to[1]) / 2) * scale, { steps: 4 });
  await page.mouse.move(box.x + to[0] * scale, box.y + to[1] * scale, { steps: 4 });
  await page.mouse.up();
  await editor.getByRole("button", { name: "Save zone" }).click();
  await expect(editor).toHaveCount(0);
  await expect(page.getByRole("status").filter({ hasText: "Saved." })).toBeVisible();
}

test("the live camera surface runs on the frame pipeline", async ({ page, request }) => {
  test.setTimeout(240_000);
  await page.goto("/ui/cameras/line-2");
  await expect(page.getByRole("heading", { name: "Line 2 camera" })).toBeVisible();
  const subbar = page.locator(".cam-subbar");
  await expect(subbar).toContainText("Live", { timeout: 30_000 });

  // The overlays are the pipeline's, on the frame they were found on, and they are ready within
  // one frame interval of capture at the rate the camera looks: 0.2 s at five frames a second.
  await expect.poll(async () => (await counts(request, "line-2")).pipeline.observed, { timeout: 30_000 }).toBeGreaterThan(5);
  const rate = page.locator(".cam-badge-rate");
  await expect(rate).toContainText(/Looking [\d.]+ a second · overlays \d+\.\d s behind/, { timeout: 20_000 });
  for (let read = 0; read < 5; read += 1) {
    const [, looking, behind] = /Looking ([\d.]+) a second · overlays (\d+\.\d) s behind/.exec((await rate.textContent()) ?? "") ?? [];
    expect(Number(behind)).toBeLessThanOrEqual(1 / Number(looking) + 0.05);
    await page.waitForTimeout(300);
  }
  await expect.poll(async () => (await view(request, "line-2")).stage.regions.length, { timeout: 20_000 }).toBeGreaterThan(0);
  const shown = await view(request, "line-2");
  const found = rfDetr ? "clock" : "part";
  expect(shown.stage.regions.some((region: { label: string; track?: number }) => region.label === found && region.track != null)).toBe(true);
  await expect(page.getByRole("complementary").getByText(/^T\d+$/).first()).toBeVisible();

  // Line 3's 2,000 frames a day cannot last at five a second: its governor looks less often,
  // stops asking sam-3.1, and the board says so from those numbers.
  await page.goto("/ui/cameras/line-3");
  await expect(page.getByRole("heading", { name: "Line 3 camera" })).toBeVisible();
  await expect(subbar).toContainText("Saving frames", { timeout: 30_000 });
  await expect(page.getByText("Saving frames.").first()).toBeVisible();
  const budget = page.getByRole("complementary");
  await expect(budget).toContainText("Looked less often: 5 → 1 a second");
  await expect(budget).toContainText("Stopped asking sam-3.1");
  const line3 = await view(request, "line-3");
  const frames = line3.budget.meters.find((meter: { label: string }) => meter.label === "Frames today");
  expect(frames.value).toMatch(/^\d[\d,]* of 2,000$/);

  await page.goto("/ui/cameras/line-2");
  await expect(subbar).toContainText("Live", { timeout: 30_000 });
  const width = (await view(request, "line-2")).stage.width as number;
  if (rfDetr) {
    // The zone starts over the belt, where RF-DETR finds nothing it watches for.
    const before = (await counts(request, "line-2")).pipeline.fired;
    await page.waitForTimeout(8_000);
    expect((await counts(request, "line-2")).pipeline.fired).toBe(before);
    // Moved over the wall clock, the clock fires from the next frames.
    await drawZone(page, [470, 5], [630, 125], width);
    await expect.poll(async () => (await counts(request, "line-2")).pipeline.fired, { timeout: 30_000 }).toBeGreaterThan(before);
    // Back over the belt, it fires no more.
    await drawZone(page, [200, 140], [460, 282], width);
    const onBelt = (await counts(request, "line-2")).pipeline.fired;
    await page.waitForTimeout(8_000);
    expect((await counts(request, "line-2")).pipeline.fired).toBe(onBelt);
  } else {
    // A crack inside the inspection window wakes the Inspector.
    await expect.poll(async () => (await counts(request, "line-2")).pipeline.fired, { timeout: 60_000 }).toBeGreaterThan(0);
    // Moved up onto the wall, the zone holds no crack: nothing fires.
    await drawZone(page, [20, 10], [600, 110], width);
    const onWall = (await counts(request, "line-2")).pipeline.fired;
    await page.waitForTimeout(15_000);
    expect((await counts(request, "line-2")).pipeline.fired).toBe(onWall);
    expect((await view(request, "line-2")).zones[0].corners[3]).toBeLessThanOrEqual(112);
    // Back over the belt, cracks fire again from the next frame.
    await drawZone(page, [200, 140], [460, 282], width);
    await expect.poll(async () => (await counts(request, "line-2")).pipeline.fired, { timeout: 60_000 }).toBeGreaterThan(onWall);
  }

  // Switched off, the camera's source stops: its RTSP connection closes before Off answers, so no
  // frame arrives and the pipeline looks at nothing.
  const cameraSwitch = page.getByRole("group", { name: "Line 2 camera: on or off" });
  await cameraSwitch.getByRole("button", { name: "Off" }).click();
  await expect(subbar).toContainText("Camera off");
  await expect(page.getByText("Nothing is captured or sent.").first()).toBeVisible();
  const off = await counts(request, "line-2");
  expect(off.on).toBe(false);
  expect(off.capturing).toBe(false);
  await page.waitForTimeout(3_000);
  const later = await counts(request, "line-2");
  expect(later.pipeline.observed).toBe(off.pipeline.observed);
  expect(later.pipeline.arrived).toBe(off.pipeline.arrived);
  expect(later.capturing).toBe(false);
  await cameraSwitch.getByRole("button", { name: "Camera on" }).click();
  await expect(subbar).toContainText("Live", { timeout: 20_000 });
  await expect.poll(async () => (await counts(request, "line-2")).pipeline.observed, { timeout: 20_000 }).toBeGreaterThan(later.pipeline.observed);

  // A card raised from the frame on screen keeps that frame and holds the line stop with it.
  await expect(page.getByRole("button", { name: "Raise a card from this frame" })).toBeEnabled({ timeout: 20_000 });
  await page.getByRole("button", { name: "Raise a card from this frame" }).click();
  const card = page.getByRole("dialog", { name: "Raise a card from this frame" });
  await card.getByRole("textbox").fill("Check this part before it leaves the line.");
  await card.getByRole("button", { name: "Raise the card" }).click();
  await expect(page.getByText(/Raised to the Inspector/)).toBeVisible({ timeout: 20_000 });
  await expect(page.getByRole("complementary").getByText("Stop line 2")).toBeVisible({ timeout: 30_000 });
  const decisions = await (await request.get("/api/surface/decisions?surface=web")).json();
  const stop = decisions.decisions.find((one: { tool?: string; heading?: string }) => JSON.stringify(one).includes("stop_line"));
  expect(stop).toBeTruthy();
  const image = stop.media.images[0];
  expect(image.digest).toMatch(/^sha256:[0-9a-f]{64}$/);
  const evidence = await request.get(image.href);
  expect(evidence.status()).toBe(200);
  expect(evidence.headers()["content-type"]).toBe("image/png");
});
