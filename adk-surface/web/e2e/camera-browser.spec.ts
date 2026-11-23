// The web home's own camera feeds the live camera surface with no voice screen and no realtime
// model (spec 3.7 §8.2, §7.13 `SurfaceCameraSource`): Chromium's fake camera stands in for this
// browser's. "Use this browser's camera" switches the camera on and sends its frames to the
// camera's pipeline; Off stops the browser's camera, so no frame is sent; a camera switched off
// from anywhere else stops it too, because the server answers that it dropped the frame.
import { expect, test } from "@playwright/test";
import type { APIRequestContext, Page } from "@playwright/test";

interface Counts {
  pipeline: { arrived: number; dropped: number; observed: number; fired: number };
  hubDropped: number;
  on: boolean;
}

async function counts(request: APIRequestContext): Promise<Counts> {
  const all = (await (await request.get("/e2e/cameras")).json()) as Record<string, Counts>;
  return all["this-computer"];
}

/** Counts the frames this page sends to the camera from now on. */
function sent(page: Page) {
  const seen = { frames: 0 };
  page.on("request", (sentRequest) => {
    if (sentRequest.method() === "POST" && /\/api\/surface\/cameras\/this-computer\/frames/.test(sentRequest.url())) seen.frames += 1;
  });
  return seen;
}

test("this browser's camera feeds its camera on the frame pipeline", async ({ page, request }) => {
  test.setTimeout(120_000);
  const seen = sent(page);
  await page.goto("/ui/cameras/this-computer");
  await expect(page.getByRole("heading", { name: "This computer's camera" })).toBeVisible();
  const subbar = page.locator(".cam-subbar");
  await expect(subbar).toContainText("Camera off");
  const before = await counts(request);
  expect(before.on).toBe(false);

  // On: the camera is switched on and the browser's frames reach the pipeline.
  const use = page.getByRole("button", { name: "Use this browser's camera" });
  await use.click();
  await expect(page.getByText("This browser's camera is sending frames.", { exact: false })).toBeVisible();
  await expect(subbar).toContainText("Live", { timeout: 20_000 });
  await expect.poll(async () => (await counts(request)).pipeline.observed, { timeout: 30_000 }).toBeGreaterThan(before.pipeline.observed + 3);
  expect((await counts(request)).on).toBe(true);
  // The picture is the pipeline's: a frame the server holds, served by its digest.
  const view = await (await request.get("/api/surface/cameras/this-computer?surface=web")).json();
  expect(view.stage.frame.digest).toMatch(/^sha256:[0-9a-f]{64}$/);
  expect(view.stage.frame.href).toContain("/api/surface/cameras/this-computer/frames/");
  expect((await request.get(view.stage.frame.href)).status()).toBe(200);
  await expect(page.locator(".cam-badge-rate")).toContainText(/Looking [\d.]+ a second/, { timeout: 20_000 });

  // Off on this page: the browser's camera stops first, and no frame is sent after it.
  await page.getByRole("group", { name: "This computer's camera: on or off" }).getByRole("button", { name: "Off" }).click();
  await expect(subbar).toContainText("Camera off");
  await expect(use).toBeVisible();
  await page.waitForTimeout(500);
  const afterOff = seen.frames;
  const offCounts = await counts(request);
  await page.waitForTimeout(2_000);
  expect(seen.frames).toBe(afterOff);
  expect((await counts(request)).pipeline.arrived).toBe(offCounts.pipeline.arrived);

  // On again from the page; then switched off elsewhere: the server answers that it dropped the
  // frame, and the page stops its camera.
  await use.click();
  await expect.poll(async () => (await counts(request)).pipeline.observed, { timeout: 30_000 }).toBeGreaterThan(offCounts.pipeline.observed + 2);
  expect((await request.post("/api/surface/cameras/this-computer/off?surface=web")).status()).toBe(200);
  await expect(use).toBeVisible({ timeout: 10_000 });
  await page.waitForTimeout(500);
  const stopped = seen.frames;
  await page.waitForTimeout(2_000);
  expect(seen.frames).toBe(stopped);
  await expect(subbar).toContainText("Camera off", { timeout: 10_000 });
});
