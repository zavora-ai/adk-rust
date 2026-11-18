// A 3D scene an agent drew (spec 3.3 §6.1 `genui`, extension catalogs, §7.4 rule 5), end to end
// over `examples/scripted_genui_scenes.rs`: Logistics draws a yard of docks in an extension
// catalog; the home draws it with WebGL (SwiftShader here), the server's summary and pick note
// over it, no decision colours or pills inside; clicking a dock fills the Dock picker and says
// so, and sends nothing; Reserve asks for one `reserve_dock` call with that dock, which runs
// once on Allow once. Without WebGL the yard is its words. `GENUI_E2E_SHOTS` names a folder for
// light and dark screenshots.
import { existsSync, mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

const workspace = process.env.GENUI_SCENES_E2E_WORKSPACE ?? "";
const shots = process.env.GENUI_E2E_SHOTS;

async function shoot(page: Page, target: Locator, name: string) {
  if (!shots) return;
  mkdirSync(shots, { recursive: true });
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.waitForTimeout(300);
    await target.screenshot({ path: join(shots, `${name}-${scheme}.png`) });
  }
  await page.emulateMedia({ colorScheme: "light" });
}

/** The yard: drawn by the first message, or already in the thread the home opens. */
async function draw(page: Page) {
  await page.goto("/ui/");
  const main = page.getByRole("main");
  const drawing = main.locator(".drawn");
  const box = page.getByRole("textbox", { name: "Message your agents" });
  await expect(box.or(drawing)).toBeVisible({ timeout: 20_000 });
  if (await box.isVisible()) {
    await box.fill("Which dock is free in yard B?");
    await box.press("Enter");
  }
  await expect(drawing).toHaveCount(1, { timeout: 20_000 });
  await expect(main.getByText("Here is the yard. Pick a free dock and press Reserve.")).toBeVisible();
  return { main, drawing, scene: drawing.locator("figure.drawn-scene") };
}

test("a yard drawn in 3D picks a dock into the form, which asks before it reserves", async ({ page }) => {
  const { main, drawing, scene } = await draw(page);
  await expect(scene).toHaveAttribute("data-scene", "3d");
  await expect(scene.locator("figcaption")).toHaveText(/^3D scene of Dock 1 · loading, Dock 2 · loading/);
  await expect(scene.getByText("Picking one fills Dock. Nothing is sent.")).toBeVisible();
  for (const word of ["OK", "Warning", "Critical"]) await expect(scene.locator(".drawn-scene-legend")).toContainText(word);
  const stage = scene.locator(".drawn-scene-stage");
  await expect(stage.locator("canvas")).toBeVisible({ timeout: 20_000 });
  const label = stage.locator(".drawn-scene-label", { hasText: "Dock 3 · free" });
  await expect(label).toHaveCSS("visibility", "visible");

  // The canvas drew something: the stage looks different with it hidden.
  await page.getByRole("button", { name: "Pause motion" }).click();
  await page.waitForTimeout(300);
  const drawn = await stage.screenshot();
  await stage.locator("canvas").evaluate((canvas: HTMLCanvasElement) => (canvas.style.visibility = "hidden"));
  const bare = await stage.screenshot();
  await stage.locator("canvas").evaluate((canvas: HTMLCanvasElement) => (canvas.style.visibility = ""));
  expect(drawn.equals(bare)).toBe(false);

  // No decision colours, classes or pills inside the drawing, light or dark.
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    const inside = await drawing.evaluate((element) =>
      [element, ...element.querySelectorAll("*")]
        .map((child) => child.className.toString())
        .filter((name) => /decision|pill/.test(name)),
    );
    expect(inside, scheme).toEqual([]);
  }
  await page.emulateMedia({ colorScheme: "light" });
  await shoot(page, drawing, "genui-scene");

  // A click on Dock 3, just under its label, picks it: the picker shows it and it is said.
  const at = await label.boundingBox();
  expect(at).not.toBeNull();
  await page.mouse.click(at!.x + at!.width / 2, at!.y + at!.height + 22);
  await expect(drawing.getByLabel("Dock 3", { exact: true })).toBeChecked();
  await expect(scene.locator('[aria-live="polite"]')).toHaveText("Picked: Dock 3 · free");
  expect(existsSync(join(workspace, "reserved.txt"))).toBe(false);
  await shoot(page, drawing, "genui-scene-picked");

  // Reserve asks first, with the dock picked; Allow once runs it once.
  await drawing.getByRole("button", { name: "Reserve" }).click();
  const card = main.getByRole("article", { name: /reserve_dock|Reserve dock/ });
  await expect(card).toBeVisible({ timeout: 20_000 });
  await expect(card.getByText(/You pressed Send on a form Logistics drew/)).toBeVisible();
  await expect(card).toContainText('"dock-3"');
  await expect(card.getByRole("button", { name: /Always allow/ })).toHaveCount(0);
  expect(existsSync(join(workspace, "reserved.txt"))).toBe(false);
  await card.getByRole("button", { name: "Allow once" }).click();
  await expect(drawing.getByText(/Sent · done at/)).toBeVisible({ timeout: 20_000 });
  const reserved = readFileSync(join(workspace, "reserved.txt"), "utf8").trim().split("\n");
  expect(reserved).toHaveLength(1);
  expect(JSON.parse(reserved[0])).toEqual({ dock: ["dock-3"] });
});

test("without WebGL the yard is its words", async ({ page }) => {
  await page.addInitScript(() => {
    const original = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function (this: HTMLCanvasElement, kind: string, ...rest: unknown[]) {
      if (kind === "webgl" || kind === "webgl2") return null;
      return original.call(this, kind as "2d", ...(rest as []));
    } as typeof original;
  });
  const { scene } = await draw(page);
  await expect(scene).toHaveAttribute("data-scene", "text");
  await expect(scene.locator("canvas")).toHaveCount(0);
  await expect(scene.getByText("3D isn't available here, so this is shown as text")).toBeVisible();
  await expect(scene.locator(".drawn-scene-objects")).toContainText("Dock 3 · free");
  await expect(scene.getByRole("button", { name: "Show in 3D" })).toHaveCount(0);
});
